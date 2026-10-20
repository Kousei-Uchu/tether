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
use tether_core::{
    identity::Identity,
    pairing::PairingOffer,
    paths::{default_device_name, Paths},
    permissions::PermissionSet,
    proto::{Permission, DEFAULT_PORT},
    session::{accept, connect, ClientMode, HostConfig, Outcome, Session},
    trust::TrustStore,
};
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

fn with_default_port(addr: &str) -> String {
    if addr.parse::<std::net::IpAddr>().is_ok() {
        return if addr.contains(':') {
            format!("[{addr}]:{DEFAULT_PORT}")
        } else {
            format!("{addr}:{DEFAULT_PORT}")
        };
    }
    if addr.starts_with('[') || addr.contains(':') {
        addr.to_owned()
    } else {
        format!("{addr}:{DEFAULT_PORT}")
    }
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

#[napi(object)]
pub struct PairingOptions {
    /// Permissions for the device that pairs with the code. Defaults to view only.
    pub grant: Option<Vec<String>>,
    /// Seconds the code stays valid. Defaults to 300.
    pub ttl_secs: Option<u32>,
}

#[napi(object)]
pub struct HostEvent {
    /// `paired`, `session-opened`, `session-closed` or `refused`.
    pub kind: String,
    pub message: String,
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
            Outcome::Authenticated(session) => Ok(Connection::new(session)),
            Outcome::Paired(_) => Err(err("unexpected pairing result")),
        }
    }

    /// Starts accepting connections. Pass `pairing` to also show a one-time pairing code.
    #[napi]
    pub async fn listen(
        &self,
        bind: String,
        pairing: Option<PairingOptions>,
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
        let code = offer.as_ref().map(|o| o.display_code());
        let host = Arc::new(HostConfig {
            identity: self.identity.clone(),
            trust: Arc::new(StdMutex::new(self.store()?)),
            pairing: Arc::new(StdMutex::new(offer)),
            device_name: self.name.clone(),
        });

        let listener = TcpListener::bind(&bind)
            .await
            .map_err(|e| err(format!("could not bind {bind}: {e}")))?;
        let address = listener.local_addr().map_err(err)?.to_string();
        let events = Arc::new(StdMutex::new(VecDeque::new()));
        let (stop, stop_rx) = watch::channel(false);

        tokio::spawn(accept_loop(listener, host, events.clone(), stop_rx));
        Ok(HostHandle {
            address,
            code,
            events,
            stop,
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
}

impl Connection {
    fn new(session: Session<TcpStream>) -> Self {
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
        }
    }
}

#[napi]
impl Connection {
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
