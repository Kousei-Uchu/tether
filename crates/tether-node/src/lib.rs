//! Node.js bridge for Electron. Wraps `tether-core` so the desktop app never touches the
//! protocol or crypto itself: it only calls this API.
//!
//! N-API is ABI stable, so the same compiled addon loads in Node (tests) and in Electron.

#[macro_use]
extern crate napi_derive;

use napi::{Error, Result};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};
use tether_clipboard::SystemClipboard;
use tether_core::{
    discovery,
    identity::Identity,
    link::{with_default_port, PairingLink},
    pairing::PairingOffer,
    paths::{default_device_name, Paths},
    permissions::PermissionSet,
    proto::{InputEvent, Key, MouseButton, Permission},
    session::{accept, connect, ClientMode, HostConfig, Outcome, Session},
    trust::TrustStore,
};
use tether_input::EnigoInput;
use tether_media::{MediaOptions, VideoHost};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{watch, Mutex, Semaphore},
};

fn err(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}

fn parse_perms(list: &[String]) -> Result<PermissionSet> {
    list.iter().try_fold(PermissionSet::none(), |acc, s| {
        let one: PermissionSet = s.parse().map_err(err)?;
        Ok(acc.union(one))
    })
}

fn names(set: PermissionSet) -> Vec<String> {
    set.to_vec()
        .into_iter()
        .map(|p: Permission| p.name().to_owned())
        .collect()
}

/// Builds the link a host shows as a QR code for a phone to scan.
#[napi]
pub fn pairing_link(address: String, code: String, name: Option<String>) -> Result<String> {
    Ok(PairingLink::new(&address, &code, name.as_deref())
        .map_err(err)?
        .to_string())
}

/// One display that can be shared.
#[napi(object)]
pub struct DisplayJs {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

/// The displays this computer can share, primary first. Empty with no screen (a server).
#[napi]
pub fn list_displays() -> Result<Vec<DisplayJs>> {
    Ok(tether_media::list_displays()
        .map_err(err)?
        .into_iter()
        .map(|d| DisplayJs {
            id: d.id,
            name: d.name,
            width: d.width,
            height: d.height,
            x: d.x,
            y: d.y,
            primary: d.primary,
        })
        .collect())
}

#[napi(object)]
pub struct ParsedLink {
    pub address: String,
    pub code: String,
    pub name: Option<String>,
}

/// Reads a pairing link, for example one pasted into the app.
#[napi]
pub fn parse_pairing_link(link: String) -> Result<ParsedLink> {
    let l = PairingLink::parse(&link).map_err(err)?;
    Ok(ParsedLink {
        address: l.address,
        code: l.code,
        name: l.name,
    })
}

/// A paired device, or the host on the other end of a connection.
#[napi(object)]
#[derive(Clone)]
pub struct PeerSummary {
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    /// On a trust list: what the device may do here. On a connection: what the host
    /// allows this device to do.
    pub permissions: Vec<String>,
}

/// A device seen on the local network. Nothing here is authenticated; `paired` only says
/// the advertised key is one this device has paired with.
#[napi(object)]
pub struct NearbyDevice {
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    /// `host:port`, IPv4 first.
    pub addresses: Vec<String>,
    pub paired: bool,
    /// False if it speaks a different protocol version and so cannot connect.
    pub compatible: bool,
}

#[napi(object)]
pub struct PairingOptions {
    /// Permissions for the device that pairs with the code. Defaults to view only.
    pub grant: Option<Vec<String>>,
    /// Seconds the code stays valid. Defaults to 300.
    pub ttl_secs: Option<u32>,
}

/// One mouse or keyboard event. `kind` picks which other fields are used:
/// `move` (x, y in 0..1 of the display), `delta` (dx, dy pixels), `button` (button, down),
/// `scroll` (dx, dy lines, positive is down or right), `key` (code, down), `text` (text).
#[napi(object)]
pub struct InputEventJs {
    pub kind: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    /// `left`, `right`, `middle`, `back` or `forward`.
    pub button: Option<String>,
    /// A DOM `KeyboardEvent.code`, for example `KeyA` or `ArrowUp`.
    pub code: Option<String>,
    pub down: Option<bool>,
    pub text: Option<String>,
    /// With `move`: aim at this display (an id from `listRemoteDisplays`), with `x` and `y`
    /// as fractions of that display.
    pub display: Option<u32>,
}

fn to_input_event(e: InputEventJs) -> Result<InputEvent> {
    let need = |name: &str| err(format!("input event is missing `{name}`"));
    let frac = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let lines = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    Ok(match e.kind.as_str() {
        "move" => {
            let (x, y) = (
                frac(e.x.ok_or_else(|| need("x"))?),
                frac(e.y.ok_or_else(|| need("y"))?),
            );
            match e.display {
                Some(display) => InputEvent::MouseMoveOn { display, x, y },
                None => InputEvent::MouseMove { x, y },
            }
        }
        "delta" => InputEvent::MouseDelta {
            dx: lines(e.dx.ok_or_else(|| need("dx"))?),
            dy: lines(e.dy.ok_or_else(|| need("dy"))?),
        },
        "scroll" => InputEvent::Scroll {
            dx: lines(e.dx.unwrap_or(0)),
            dy: lines(e.dy.unwrap_or(0)),
        },
        "button" => {
            let name = e.button.ok_or_else(|| need("button"))?;
            InputEvent::MouseButton {
                button: MouseButton::from_name(&name)
                    .ok_or_else(|| err(format!("unknown mouse button {name:?}")))?,
                down: e.down.ok_or_else(|| need("down"))?,
            }
        }
        "key" => {
            let code = e.code.ok_or_else(|| need("code"))?;
            InputEvent::Key {
                key: Key::from_dom_code(&code)
                    .ok_or_else(|| err(format!("unsupported key {code:?}")))?,
                down: e.down.ok_or_else(|| need("down"))?,
            }
        }
        "text" => InputEvent::Text {
            text: e.text.ok_or_else(|| need("text"))?,
        },
        other => return Err(err(format!("unknown input kind {other:?}"))),
    })
}

#[napi(object)]
pub struct HostEvent {
    /// `paired`, `session-opened`, `session-closed` or `refused`.
    pub kind: String,
    pub message: String,
}

/// The host's answer to `getClipboard`.
#[napi(object)]
pub struct ClipboardReply {
    pub text: Option<String>,
    pub notice: Option<String>,
}

/// Records any Rust panic, with a backtrace, in `crash.log` in the data directory. The
/// default hook only prints to stderr, which nobody sees in a packaged app. Installed once.
fn install_crash_log(dir: &std::path::Path) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let file = dir.join("crash.log");
    ONCE.call_once(move || {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let trace = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current();
            let line = format!(
                "[{:?}] panic on thread {:?}: {info}\n{trace}\n\n",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                thread.name().unwrap_or("unnamed"),
            );
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
            {
                use std::io::Write;
                let _ = f.write_all(line.as_bytes());
            }
            previous(info);
        }));
    });
}

/// This machine's identity and trust store.
#[napi]
pub struct Device {
    paths: Paths,
    identity: Arc<Identity>,
    name: String,
}

#[napi]
impl Device {
    /// `home` overrides the data directory (used by tests to run two devices in one process).
    #[napi(constructor)]
    pub fn new(home: Option<String>, name: Option<String>) -> Result<Self> {
        let paths = Paths::resolve(home.map(PathBuf::from)).map_err(err)?;
        install_crash_log(&paths.dir);
        let identity = Identity::load_or_create(&paths.identity_file()).map_err(err)?;
        Ok(Self {
            paths,
            identity: Arc::new(identity),
            name: name.unwrap_or_else(default_device_name),
        })
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    #[napi(getter)]
    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint()
    }

    #[napi(getter)]
    pub fn home(&self) -> String {
        self.paths.dir.display().to_string()
    }

    /// Where Rust panics are written. Look here first after a crash.
    #[napi(getter)]
    pub fn crash_log(&self) -> String {
        self.paths.dir.join("crash.log").display().to_string()
    }

    fn store(&self) -> Result<TrustStore> {
        TrustStore::load(self.paths.trust_file()).map_err(err)
    }

    /// Devices paired with this machine and what each may do here.
    #[napi]
    pub fn peers(&self) -> Result<Vec<PeerSummary>> {
        Ok(self.store()?.peers().iter().map(summary).collect())
    }

    /// Adds permissions. A running host applies the change to open sessions immediately.
    #[napi]
    pub fn grant(&self, query: String, permissions: Vec<String>) -> Result<Vec<PeerSummary>> {
        let add = parse_perms(&permissions)?;
        self.change(&query, |p| p.union(add))
    }

    #[napi]
    pub fn revoke(&self, query: String, permissions: Vec<String>) -> Result<Vec<PeerSummary>> {
        let drop = parse_perms(&permissions)?;
        self.change(&query, |p| p.minus(drop))
    }

    fn change(
        &self,
        query: &str,
        f: impl Fn(PermissionSet) -> PermissionSet,
    ) -> Result<Vec<PeerSummary>> {
        let mut store = self.store()?;
        if store.change_permissions(query, f).map_err(err)? == 0 {
            return Err(err(format!("no paired device matches {query:?}")));
        }
        Ok(store
            .peers()
            .iter()
            .filter(|p| p.matches(query))
            .map(summary)
            .collect())
    }

    /// Unpairs matching devices. Returns how many were removed.
    #[napi]
    pub fn remove(&self, query: String) -> Result<u32> {
        let n = self.store()?.remove(&query).map_err(err)?;
        Ok(n as u32)
    }

    /// Pairs with a host that is showing `code`.
    #[napi]
    pub async fn pair(&self, address: String, code: String) -> Result<PeerSummary> {
        let mut store = self.store()?;
        let stream = dial(&address).await?;
        match connect(
            stream,
            &self.identity,
            &mut store,
            &self.name,
            ClientMode::Pair { code },
        )
        .await
        .map_err(err)?
        {
            Outcome::Paired(peer) => Ok(PeerSummary {
                name: peer.name.clone(),
                platform: peer.platform.to_string(),
                fingerprint: peer.fingerprint(),
                permissions: vec![],
            }),
            Outcome::Authenticated(_) => Err(err("unexpected session while pairing")),
        }
    }

    /// Looks for Tether devices on the local network for `seconds` (default 3, at most 15).
    #[napi]
    pub async fn discover(&self, seconds: Option<u32>) -> Result<Vec<NearbyDevice>> {
        let store = self.store()?;
        let secs = u64::from(seconds.unwrap_or(3).clamp(1, 15));
        let found = discovery::scan(Duration::from_secs(secs), Some(self.identity.public))
            .await
            .map_err(err)?;
        Ok(found
            .into_iter()
            .map(|d| NearbyDevice {
                paired: discovery::identify(&d, &store).is_some(),
                compatible: d.compatible(),
                addresses: d.addresses.iter().map(|a| a.to_string()).collect(),
                name: d.name,
                platform: d.platform,
                fingerprint: d.fingerprint,
            })
            .collect())
    }

    /// Finds a paired host on the network by fingerprint and opens a session with it. For
    /// when its saved address no longer works. The returned connection's `address` is where
    /// it was found.
    #[napi]
    pub async fn quick_connect(&self, fingerprint: String) -> Result<Connection> {
        let mut store = self.store()?;
        let wanted = store
            .peers()
            .iter()
            .find(|p| p.fingerprint() == fingerprint)
            .map(|p| p.public_key.clone())
            .ok_or_else(|| err("that device is not paired with this one"))?;
        let device = discovery::find(&fingerprint, Duration::from_secs(5), Some(self.identity.public))
            .await
            .map_err(err)?
            .ok_or_else(|| err("could not find that device on the network"))?;
        if hex::encode(device.public_key) != wanted {
            return Err(err("could not find that device on the network"));
        }
        let (stream, addr) = discovery::dial(&device, Duration::from_secs(4))
            .await
            .map_err(|e| err(format!("found the device but could not connect: {e}")))?;
        match connect(
            stream,
            &self.identity,
            &mut store,
            &self.name,
            ClientMode::Connect,
        )
        .await
        .map_err(err)?
        {
            Outcome::Authenticated(session) => {
                if hex::encode(session.peer.public_key) != wanted {
                    return Err(err("the device at that address is not the one you asked for"));
                }
                Ok(Connection::new(session, addr.to_string()))
            }
            Outcome::Paired(_) => Err(err("unexpected pairing result")),
        }
    }

    /// Opens an authenticated session with a paired host.
    #[napi]
    pub async fn connect(&self, address: String) -> Result<Connection> {
        let mut store = self.store()?;
        let stream = dial(&address).await?;
        match connect(
            stream,
            &self.identity,
            &mut store,
            &self.name,
            ClientMode::Connect,
        )
        .await
        .map_err(err)?
        {
            Outcome::Authenticated(session) => {
                Ok(Connection::new(session, with_default_port(address.trim())))
            }
            Outcome::Paired(_) => Err(err("unexpected pairing result")),
        }
    }

    /// Starts accepting connections. Pass `pairing` to also show a one-time pairing code, and
    /// `video` (`"test-pattern"`, `"screen"` or `"screen:<id>"`) to let permitted devices watch a stream, and
    /// `input` to let devices with the `input` permission control this computer's mouse and
    /// keyboard. Remote control is off unless the host turns it on here. `audio` (`"system"`
    /// or `"tone"`) adds sound to the video; it needs `video`. `wake` makes this device a
    /// wake relay: each entry is `name=MAC` or `name=MAC@broadcast`, and devices with the
    /// `power` permission can then wake those computers. `clipboard` does the
    /// same for devices with the `files` permission and this computer's clipboard.
    #[allow(clippy::too_many_arguments)]
    #[napi]
    pub async fn listen(
        &self,
        bind: String,
        pairing: Option<PairingOptions>,
        video: Option<String>,
        input: Option<bool>,
        clipboard: Option<bool>,
        audio: Option<String>,
        wake: Option<Vec<String>>,
        advertise: Option<bool>,
    ) -> Result<HostHandle> {
        let offer = match pairing {
            None => None,
            Some(opts) => {
                let grant = match opts.grant {
                    Some(list) => parse_perms(&list)?,
                    None => PermissionSet::default_grant(),
                };
                let ttl = Duration::from_secs(u64::from(opts.ttl_secs.unwrap_or(300)));
                Some(PairingOffer::new(ttl).with_grant(grant))
            }
        };
        let media: Option<Arc<dyn tether_core::media::MediaProvider>> = match video.as_deref() {
            None | Some("") | Some("none") => None,
            Some(spec) => {
                let spec: tether_media::VideoSpec = spec.parse().map_err(err)?;
                let mut host = VideoHost::from_spec(spec, MediaOptions::default());
                match audio.as_deref() {
                    None | Some("") | Some("none") => {}
                    Some(a) => host = host.with_audio(a.parse().map_err(err)?),
                }
                Some(Arc::new(host))
            }
        };
        let relay: Option<Arc<dyn tether_core::wake::WakeProvider>> = match wake {
            Some(specs) if !specs.is_empty() => Some(Arc::new(
                tether_core::wake::WolRelay::from_specs(&specs).map_err(err)?,
            )),
            _ => None,
        };
        let code = offer.as_ref().map(|o| o.display_code());
        let host = Arc::new(HostConfig {
            identity: self.identity.clone(),
            trust: Arc::new(StdMutex::new(self.store()?)),
            pairing: Arc::new(StdMutex::new(offer)),
            device_name: self.name.clone(),
            media,
            input: input
                .unwrap_or(false)
                .then(|| Arc::new(EnigoInput) as Arc<dyn tether_core::input::InputProvider>),
            wake: relay,
            clipboard: clipboard.unwrap_or(false).then(|| {
                Arc::new(SystemClipboard::new())
                    as Arc<dyn tether_core::clipboard::ClipboardProvider>
            }),
        });

        let listener = TcpListener::bind(&bind)
            .await
            .map_err(|e| err(format!("could not bind {bind}: {e}")))?;
        let address = listener.local_addr().map_err(err)?.to_string();
        let events = Arc::new(StdMutex::new(VecDeque::new()));
        let (stop, stop_rx) = watch::channel(false);

        // Announce on the local network unless told not to. Failing to do so (no network,
        // blocked multicast) must not stop the host from working.
        let advert = if advertise.unwrap_or(true) {
            let port = listener.local_addr().map_err(err)?.port();
            match discovery::Advertiser::start(
                &self.name,
                &self.identity.public,
                tether_core::proto::Platform::current(),
                port,
            ) {
                Ok(a) => {
                    push(&events, "info", format!("Visible to nearby devices as {}", a.instance()));
                    Some(a)
                }
                Err(e) => {
                    push(&events, "info", format!("Not visible to nearby devices: {e}"));
                    None
                }
            }
        } else {
            None
        };

        tokio::spawn(accept_loop(listener, host, events.clone(), stop_rx));
        Ok(HostHandle {
            address,
            code,
            events,
            stop,
            advert: StdMutex::new(advert),
        })
    }
}

fn summary(p: &tether_core::trust::TrustedPeer) -> PeerSummary {
    PeerSummary {
        name: p.name.clone(),
        platform: p.platform.to_string(),
        fingerprint: p.fingerprint(),
        permissions: names(p.permissions),
    }
}

async fn dial(address: &str) -> Result<TcpStream> {
    let target = with_default_port(address.trim());
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&target))
        .await
        .map_err(|_| err(format!("timed out connecting to {target}")))?
        .map_err(|e| err(format!("could not connect to {target}: {e}")))?;
    stream.set_nodelay(true).map_err(err)?;
    Ok(stream)
}

/// An open, authenticated session with a host.
#[napi]
pub struct Connection {
    session: Arc<Mutex<Option<Session<TcpStream>>>>,
    peer: PeerSummary,
    permissions: Arc<StdMutex<Vec<String>>>,
    nonce: AtomicU64,
    address: String,
}

impl Connection {
    fn new(session: Session<TcpStream>, address: String) -> Self {
        let perms = names(session.permissions);
        let peer = PeerSummary {
            name: session.peer.name.clone(),
            platform: session.peer.platform.to_string(),
            fingerprint: session.peer.fingerprint(),
            permissions: perms.clone(),
        };
        Self {
            session: Arc::new(Mutex::new(Some(session))),
            peer,
            permissions: Arc::new(StdMutex::new(perms)),
            nonce: AtomicU64::new(0),
            address,
        }
    }
}

#[napi]
impl Connection {
    /// The `host:port` this session is connected to.
    #[napi(getter)]
    pub fn address(&self) -> String {
        self.address.clone()
    }

    #[napi(getter)]
    pub fn peer(&self) -> PeerSummary {
        self.peer.clone()
    }

    /// What the host currently allows this device to do. Updated by `ping`.
    #[napi(getter)]
    pub fn permissions(&self) -> Vec<String> {
        self.permissions.lock().expect("permissions lock").clone()
    }

    /// Round trip time in milliseconds. Also picks up permission changes made on the host.
    #[napi]
    pub async fn ping(&self) -> Result<f64> {
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        let result = session.ping(nonce).await;
        *self.permissions.lock().expect("permissions lock") = names(session.permissions);
        match result {
            Ok(rtt) => Ok(rtt.as_secs_f64() * 1000.0),
            Err(e) => {
                *guard = None;
                Err(err(e))
            }
        }
    }

    /// Asks the host, which must be a wake relay, to wake the computer it knows as `target`.
    /// Needs the `power` permission. Resolves with the host's answer, which says what it did
    /// or why it would not.
    #[napi]
    pub async fn wake(&self, target: String) -> Result<String> {
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        let outcome = async {
            session.wake(&target).await?;
            // The answer arrives as a notice, ahead of the reply to this ping.
            session.ping(nonce).await?;
            Ok::<_, tether_core::session::SessionError>(session.take_notices())
        }
        .await;
        match outcome {
            Ok(notices) => notices
                .last()
                .cloned()
                .ok_or_else(|| err("the host did not answer")),
            Err(e) => {
                *guard = None;
                Err(err(e))
            }
        }
    }

    /// Asks the host for its screen. `offer` is a WebRTC session description from the
    /// viewer; the result is the host's answer. Needs the `view` permission.
    #[napi]
    pub async fn request_video(&self, offer: String) -> Result<String> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        session.request_video(offer).await.map_err(err)
    }

    /// The displays the host can stream one by one. Needs the `view` permission.
    #[napi]
    pub async fn list_remote_displays(&self) -> Result<Vec<DisplayJs>> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        let list = session.list_displays().await.map_err(err)?;
        Ok(list
            .into_iter()
            .map(|d| DisplayJs {
                id: d.id,
                name: d.name,
                width: d.width,
                height: d.height,
                x: d.x,
                y: d.y,
                primary: d.primary,
            })
            .collect())
    }

    /// Like `requestVideo`, for one display from `listRemoteDisplays`. Each display has its
    /// own connection, so a viewer can show every screen at once.
    #[napi]
    pub async fn request_display_video(&self, display: u32, offer: String) -> Result<String> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        session
            .request_display_video(display, offer)
            .await
            .map_err(err)
    }

    /// Sends one mouse or keyboard event to the host. Fire and forget: if the host refuses it,
    /// the reason arrives later through `takeNotices`.
    #[napi]
    pub async fn send_input(&self, event: InputEventJs) -> Result<()> {
        let event = to_input_event(event)?;
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        if let Err(e) = session.send_input(event).await {
            *guard = None;
            return Err(err(e));
        }
        Ok(())
    }

    /// Puts `text` on the host's clipboard. Needs the `files` permission and a host that has
    /// turned clipboard sharing on. Resolves with what the host said, for example
    /// "Copied to the host's clipboard." or why it refused.
    #[napi]
    pub async fn send_clipboard(&self, text: String) -> Result<String> {
        if text.len() > tether_core::proto::MAX_CLIPBOARD_BYTES {
            return Err(err(
                "That text is larger than 1 MiB, which is the most that can be sent.",
            ));
        }
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        // The host answers with a notice, which arrives ahead of the reply to this ping.
        let result = match session.send_clipboard(&text).await {
            Ok(()) => session.ping(nonce).await.map(|_| ()),
            Err(e) => Err(e),
        };
        *self.permissions.lock().expect("permissions lock") = names(session.permissions);
        match result {
            Ok(()) => {
                let notices = session.take_notices();
                if notices.is_empty() {
                    Err(err("the host did not answer"))
                } else {
                    Ok(notices.join(" "))
                }
            }
            Err(e) => {
                *guard = None;
                Err(err(e))
            }
        }
    }

    /// The text on the host's clipboard. Needs the `files` permission and a host that has
    /// turned clipboard sharing on. `text` is null, with the reason in `notice`, if the host
    /// refused or has no text on its clipboard.
    #[napi]
    pub async fn get_clipboard(&self) -> Result<ClipboardReply> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| err("connection is closed"))?;
        let result = session.get_clipboard().await;
        *self.permissions.lock().expect("permissions lock") = names(session.permissions);
        match result {
            Ok(text) => {
                let notices = session.take_notices();
                Ok(ClipboardReply {
                    text,
                    notice: (!notices.is_empty()).then(|| notices.join(" ")),
                })
            }
            Err(e) => {
                *guard = None;
                Err(err(e))
            }
        }
    }

    /// Messages from the host meant for the person, such as "remote control is turned off".
    /// They are collected while waiting for replies, so call this after `ping`.
    #[napi]
    pub async fn take_notices(&self) -> Vec<String> {
        match self.session.lock().await.as_mut() {
            Some(session) => session.take_notices(),
            None => Vec::new(),
        }
    }

    #[napi]
    pub async fn close(&self) -> Result<()> {
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            let _ = session.bye("closed").await;
        }
        Ok(())
    }
}

/// A running host. Dropping it does not stop it; call `stop`.
#[napi]
pub struct HostHandle {
    address: String,
    code: Option<String>,
    events: Arc<StdMutex<VecDeque<(String, String)>>>,
    stop: watch::Sender<bool>,
    /// Dropping this withdraws the network announcement.
    advert: StdMutex<Option<discovery::Advertiser>>,
}

#[napi]
impl HostHandle {
    /// The address actually bound, for example `127.0.0.1:7420`.
    #[napi(getter)]
    pub fn address(&self) -> String {
        self.address.clone()
    }

    #[napi(getter)]
    pub fn pairing_code(&self) -> Option<String> {
        self.code.clone()
    }

    /// Returns and clears what happened since the last call.
    #[napi]
    pub fn drain_events(&self) -> Vec<HostEvent> {
        let mut q = self.events.lock().expect("events lock");
        q.drain(..)
            .map(|(kind, message)| HostEvent { kind, message })
            .collect()
    }

    #[napi]
    /// Stops accepting and closes open sessions.
    pub fn stop(&self) {
        self.stop.send_replace(true);
        self.advert.lock().expect("advert lock").take();
    }
}

type Events = Arc<StdMutex<VecDeque<(String, String)>>>;

fn push(events: &Events, kind: &str, message: String) {
    let mut q = events.lock().expect("events lock");
    if q.len() >= 200 {
        q.pop_front();
    }
    q.push_back((kind.to_owned(), message));
}

async fn accept_loop(
    listener: TcpListener,
    host: Arc<HostConfig>,
    events: Events,
    mut stop: watch::Receiver<bool>,
) {
    let limit = Arc::new(Semaphore::new(16));
    let session_stop = stop.clone();
    loop {
        tokio::select! {
            _ = stop.wait_for(|stopped| *stopped) => return,
            res = listener.accept() => {
                let Ok((stream, remote)) = res else { continue };
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
                let _ = stream.set_nodelay(true);
                let (host, events, mut stop) = (host.clone(), events.clone(), session_stop.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    tokio::select! {
                        _ = stop.wait_for(|stopped| *stopped) => {}
                        _ = handle(stream, remote.to_string(), host, events) => {}
                    }
                });
            }
        }
    }
}

async fn handle(stream: TcpStream, remote: String, host: Arc<HostConfig>, events: Events) {
    match accept(stream, &host).await {
        Ok(Outcome::Authenticated(session)) => {
            let who = format!("{} ({})", session.peer.name, session.peer.fingerprint());
            push(
                &events,
                "session-opened",
                format!("{who} connected from {remote}"),
            );
            let note = match session.serve(&host).await {
                Ok(()) => format!("{who} disconnected"),
                Err(e) => format!("{who} session ended: {e}"),
            };
            push(&events, "session-closed", note);
        }
        Ok(Outcome::Paired(peer)) => {
            push(
                &events,
                "paired",
                format!("paired with {} ({})", peer.name, peer.fingerprint()),
            );
        }
        Err(e) => push(&events, "refused", format!("{remote}: {e}")),
    }
}
