//! Authentication, pairing and permissions on top of the secure channel.
//!
//! Flow (see docs/SPEC.md section 5):
//!   Noise XX handshake -> Hello both ways -> either `Ready` + `Permissions` (paired client)
//!   or a one-shot pairing exchange (`PairRequest` / `PairAccept`).

use crate::{
    channel::{ChannelError, Role, SecureChannel, SecureSender},
    clipboard::ClipboardProvider,
    identity::{fingerprint, Identity},
    input::{InputProvider, InputSink},
    media::MediaProvider,
    pairing::{self, PairingOffer, ProofRole},
    permissions::{PermissionDenied, PermissionSet},
    trust::{TrustStore, TrustedPeer},
    wake::WakeProvider,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tether_proto::{
    DisplayDesc, Hello, InputEvent, Intent, Message, Permission, Platform, SignalKind,
    MAX_CLIPBOARD_BYTES, PROTOCOL_VERSION,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    time::{interval, timeout, MissedTickBehavior},
};

pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// A host drops a client it has not heard from for this long. Clients keep the session
/// alive with pings, including while video is playing.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a host re-checks a connected device against the trust file. This bounds how long
/// a revoked permission, such as `view`, keeps working on a session that is otherwise quiet.
pub const PERMISSION_POLL: Duration = Duration::from_secs(1);
/// Pointer moves and scrolls beyond this many per second are dropped by the host. Presses and
/// releases are never dropped.
pub const MAX_DROPPABLE_INPUT_PER_SEC: u32 = 1500;
/// After the host fails to open its input injector, it waits this long before trying again.
const INPUT_RETRY: Duration = Duration::from_secs(2);
/// Least time between wake signals from one session, so a stuck button cannot flood the LAN.
/// How long a host reuses its list of displays while translating pointer moves.
const DISPLAY_CACHE_TTL: Duration = Duration::from_secs(2);
const WAKE_COOLDOWN: Duration = Duration::from_secs(3);
/// How long a client waits for the host to answer a video offer.
pub const SIGNAL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Channel(#[from] ChannelError),
    #[error("the other device refused the connection: {0}")]
    Rejected(String),
    #[error("this host is not paired (fingerprint {fingerprint}); run `tether pair` first")]
    HostNotTrusted { fingerprint: String },
    #[error("a device with fingerprint {fingerprint} tried to connect but is not paired")]
    PeerNotTrusted { fingerprint: String },
    #[error("this device's access was revoked")]
    Revoked,
    #[error(transparent)]
    PermissionDenied(#[from] PermissionDenied),
    #[error("protocol version mismatch (ours {ours}, theirs {theirs})")]
    VersionMismatch { ours: u16, theirs: u16 },
    #[error("pairing failed: {0}")]
    PairingFailed(String),
    #[error("invalid pairing code; expected 16 letters and digits like XXXX-XXXX-XXXX-XXXX")]
    InvalidCode,
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    #[error("the host could not start video: {0}")]
    Signal(String),
    #[error("timed out")]
    Timeout,
    #[error("could not read or update the trust store: {0}")]
    Store(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    pub platform: Platform,
    pub public_key: [u8; 32],
}

impl PeerInfo {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}

pub struct Session<S> {
    pub channel: SecureChannel<S>,
    pub peer: PeerInfo,
    /// On a host: what the connected device may do. On a client: what the host allows this
    /// device to do, kept current from `Permissions` messages (use it to grey out controls).
    pub permissions: PermissionSet,
    /// Host side: the running video session, if any. Dropping it stops the video.
    media_guard: Option<Box<dyn Send>>,
    /// Client side: notices the host sent, until `take_notices` collects them.
    notices: Vec<String>,
}

pub enum Outcome<S> {
    /// Normal authenticated session; keep using `channel`.
    Authenticated(Session<S>),
    /// Pairing finished and both sides stored each other. The connection is done.
    Paired(PeerInfo),
}

/// Everything a host needs to accept connections.
pub struct HostConfig {
    pub identity: Arc<Identity>,
    pub trust: Arc<Mutex<TrustStore>>,
    pub pairing: Arc<Mutex<Option<PairingOffer>>>,
    pub device_name: String,
    /// Where video comes from. `None` means this host does not share its screen.
    pub media: Option<Arc<dyn MediaProvider>>,
    /// Whether this host lets devices control its mouse and keyboard. `None` means no, even
    /// for a device that holds the `input` permission.
    pub input: Option<Arc<dyn InputProvider>>,
    /// Wake-on-LAN targets this host can wake for a device holding `power`. `None` means
    /// this host is not a wake relay.
    pub wake: Option<Arc<dyn WakeProvider>>,
    /// Whether this host lets devices read and replace its clipboard. `None` means no, even
    /// for a device that holds the `files` permission.
    pub clipboard: Option<Arc<dyn ClipboardProvider>>,
}

impl HostConfig {
    /// Current permissions of a device, or `None` if it is not (or no longer) paired.
    /// Reloads the trust file first if another process changed it.
    pub fn permissions_for(&self, key: &[u8; 32]) -> std::io::Result<Option<PermissionSet>> {
        let mut trust = self.trust.lock().expect("trust lock");
        trust.refresh()?;
        Ok(trust.get(key).map(|p| p.permissions))
    }
}

fn hello(name: &str, intent: Intent) -> Message {
    Message::Hello(Hello {
        version: PROTOCOL_VERSION,
        device_name: name.to_owned(),
        platform: Platform::current(),
        intent,
    })
}

fn check_hello(msg: Message) -> Result<Hello, SessionError> {
    match msg {
        Message::Hello(h) if h.version == PROTOCOL_VERSION => Ok(h),
        Message::Hello(h) => Err(SessionError::VersionMismatch {
            ours: PROTOCOL_VERSION,
            theirs: h.version,
        }),
        Message::Reject { reason } | Message::Bye { reason } => Err(SessionError::Rejected(reason)),
        _ => Err(SessionError::Protocol("expected hello")),
    }
}

/// Host side: authenticate one inbound connection.
pub async fn accept<S>(stream: S, host: &HostConfig) -> Result<Outcome<S>, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(AUTH_TIMEOUT, accept_inner(stream, host))
        .await
        .map_err(|_| SessionError::Timeout)?
}

async fn accept_inner<S>(stream: S, host: &HostConfig) -> Result<Outcome<S>, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ch = SecureChannel::handshake(stream, &host.identity, Role::Responder).await?;
    let remote_key = *ch.remote_static();

    let theirs = check_hello(ch.recv().await?)?;
    ch.send(&hello(&host.device_name, Intent::Connect)).await?;
    let peer = PeerInfo {
        name: theirs.device_name.clone(),
        platform: theirs.platform,
        public_key: remote_key,
    };

    match theirs.intent {
        Intent::Connect => {
            let Some(permissions) = host.permissions_for(&remote_key)? else {
                ch.send(&Message::Reject {
                    reason: "this device is not paired".into(),
                })
                .await?;
                return Err(SessionError::PeerNotTrusted {
                    fingerprint: peer.fingerprint(),
                });
            };
            ch.send(&Message::Ready).await?;
            ch.send(&Message::Permissions {
                granted: permissions.to_vec(),
            })
            .await?;
            Ok(Outcome::Authenticated(Session {
                channel: ch,
                peer,
                permissions,
                media_guard: None,
                notices: Vec::new(),
            }))
        }
        Intent::Pair => {
            let Message::PairRequest { proof } = ch.recv().await? else {
                return Err(SessionError::Protocol("expected pair request"));
            };
            match consume_pairing(host, &proof, ch.handshake_hash()) {
                Ok((host_proof, grant)) => {
                    host.trust
                        .lock()
                        .expect("trust lock")
                        .add(TrustedPeer::new(
                            peer.name.clone(),
                            &remote_key,
                            peer.platform,
                            grant,
                        ))?;
                    ch.send(&Message::PairAccept { proof: host_proof }).await?;
                    Ok(Outcome::Paired(peer))
                }
                Err(reason) => {
                    ch.send(&Message::Reject {
                        reason: reason.clone(),
                    })
                    .await?;
                    Err(SessionError::PairingFailed(reason))
                }
            }
        }
    }
}

/// Checks the proof against the active offer. Succeeds at most once per offer. The offer is
/// dropped on success, on expiry, and after too many bad guesses. On success returns the
/// host's proof and the permissions the offer grants.
fn consume_pairing(
    host: &HostConfig,
    proof: &[u8; 32],
    handshake_hash: &[u8],
) -> Result<([u8; 32], PermissionSet), String> {
    let mut slot = host.pairing.lock().expect("pairing lock");
    let Some(mut offer) = slot.take() else {
        return Err("pairing is not enabled on this device".into());
    };
    if Instant::now() >= offer.expires_at {
        return Err("the pairing code has expired".into());
    }
    if !pairing::verify(offer.code(), ProofRole::Client, handshake_hash, proof) {
        offer.attempts_left = offer.attempts_left.saturating_sub(1);
        if offer.attempts_left > 0 {
            *slot = Some(offer);
        }
        return Err("pairing failed".into());
    }
    let host_proof = pairing::proof(offer.code(), ProofRole::Host, handshake_hash);
    Ok((host_proof, offer.grant))
}

pub enum ClientMode {
    /// Connect to an already paired host.
    Connect,
    /// Pair with a host that is showing this code.
    Pair { code: String },
}

/// Client side: connect to a host, either as a normal session or to pair.
pub async fn connect<S>(
    stream: S,
    identity: &Identity,
    trust: &mut TrustStore,
    device_name: &str,
    mode: ClientMode,
) -> Result<Outcome<S>, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(
        AUTH_TIMEOUT,
        connect_inner(stream, identity, trust, device_name, mode),
    )
    .await
    .map_err(|_| SessionError::Timeout)?
}

async fn connect_inner<S>(
    stream: S,
    identity: &Identity,
    trust: &mut TrustStore,
    device_name: &str,
    mode: ClientMode,
) -> Result<Outcome<S>, SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ch = SecureChannel::handshake(stream, identity, Role::Initiator).await?;
    let host_key = *ch.remote_static();

    match mode {
        ClientMode::Connect => {
            trust.refresh()?;
            if !trust.contains(&host_key) {
                let _ = ch
                    .send(&Message::Bye {
                        reason: "host not trusted".into(),
                    })
                    .await;
                return Err(SessionError::HostNotTrusted {
                    fingerprint: fingerprint(&host_key),
                });
            }
            ch.send(&hello(device_name, Intent::Connect)).await?;
            let theirs = check_hello(ch.recv().await?)?;
            match ch.recv().await? {
                Message::Ready => {}
                Message::Reject { reason } => return Err(SessionError::Rejected(reason)),
                _ => return Err(SessionError::Protocol("expected ready")),
            }
            let Message::Permissions { granted } = ch.recv().await? else {
                return Err(SessionError::Protocol("expected permissions"));
            };
            let peer = PeerInfo {
                name: theirs.device_name,
                platform: theirs.platform,
                public_key: host_key,
            };
            Ok(Outcome::Authenticated(Session {
                channel: ch,
                peer,
                permissions: granted.into(),
                media_guard: None,
                notices: Vec::new(),
            }))
        }
        ClientMode::Pair { code } => {
            let code = pairing::normalize_code(&code).ok_or(SessionError::InvalidCode)?;
            ch.send(&hello(device_name, Intent::Pair)).await?;
            let theirs = check_hello(ch.recv().await?)?;

            let proof = pairing::proof(&code, ProofRole::Client, ch.handshake_hash());
            ch.send(&Message::PairRequest { proof }).await?;
            match ch.recv().await? {
                Message::PairAccept { proof } => {
                    if !pairing::verify(&code, ProofRole::Host, ch.handshake_hash(), &proof) {
                        return Err(SessionError::PairingFailed(
                            "the host could not prove it knows the code".into(),
                        ));
                    }
                }
                Message::Reject { reason } => return Err(SessionError::PairingFailed(reason)),
                _ => return Err(SessionError::Protocol("expected pair accept")),
            }
            let peer = PeerInfo {
                name: theirs.device_name,
                platform: theirs.platform,
                public_key: host_key,
            };
            // A client keeps the host as identity only; permissions apply to the host's side.
            trust.add(TrustedPeer::new(
                peer.name.clone(),
                &host_key,
                peer.platform,
                PermissionSet::none(),
            ))?;
            Ok(Outcome::Paired(peer))
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Round trip time of one ping. Also applies any `Permissions` update that arrives.
    pub async fn ping(&mut self, nonce: u64) -> Result<Duration, SessionError> {
        let start = Instant::now();
        if let Err(e) = self.channel.send(&Message::Ping { nonce }).await {
            // The host may have said goodbye (for example "access revoked") and hung up.
            return Err(match self.read_goodbye().await {
                Some(reason) => SessionError::Rejected(reason),
                None => e.into(),
            });
        }
        loop {
            match timeout(AUTH_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::Pong { nonce: n } if n == nonce => return Ok(start.elapsed()),
                Message::Permissions { granted } => self.permissions = granted.into(),
                Message::Notice { text } => self.push_notice(text),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
            }
        }
    }

    fn push_notice(&mut self, text: String) {
        if self.notices.len() < 20 {
            self.notices.push(text);
        }
    }

    /// Messages the host sent for the person to read, such as "remote control is turned off".
    /// They arrive while waiting for replies, so call this after `ping`.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Client side: sends one mouse or keyboard event. The host drops it, and says why in a
    /// notice, if this device lacks the permission or the host has not enabled remote control.
    pub async fn send_input(&mut self, event: InputEvent) -> Result<(), SessionError> {
        self.channel.send(&Message::Input(event)).await?;
        Ok(())
    }

    /// Client side: the displays the host can stream one by one. Needs `view`.
    pub async fn list_displays(&mut self) -> Result<Vec<DisplayDesc>, SessionError> {
        self.channel.send(&Message::ListDisplays).await?;
        loop {
            match timeout(SIGNAL_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::Displays { list } => return Ok(list),
                Message::Permissions { granted } => self.permissions = granted.into(),
                Message::Notice { text } => self.push_notice(text),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
            }
        }
    }

    /// Client side: like `request_video`, for one display from `list_displays`. Each display
    /// gets its own WebRTC connection, so call this once per screen.
    pub async fn request_display_video(
        &mut self,
        display: u32,
        offer: String,
    ) -> Result<String, SessionError> {
        self.channel
            .send(&Message::DisplaySignal {
                display,
                kind: SignalKind::Offer,
                payload: offer,
            })
            .await?;
        loop {
            match timeout(SIGNAL_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::DisplaySignal {
                    display: d,
                    kind: SignalKind::Answer,
                    payload,
                } if d == display => return Ok(payload),
                Message::DisplaySignal {
                    display: d,
                    kind: SignalKind::Error,
                    payload,
                } if d == display => return Err(SessionError::Signal(payload)),
                Message::Permissions { granted } => self.permissions = granted.into(),
                Message::Notice { text } => self.push_notice(text),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
            }
        }
    }

    /// Client side: asks the host to wake the computer it knows as `target`. The answer
    /// arrives as a notice, so call `ping` and then `take_notices`.
    pub async fn wake(&mut self, target: &str) -> Result<(), SessionError> {
        self.channel
            .send(&Message::Wake {
                target: target.to_owned(),
            })
            .await?;
        Ok(())
    }

    /// Client side: puts `text` on the host's clipboard. Needs `files`. The host answers with
    /// a notice (done, refused or turned off), so call `ping` and then `take_notices`.
    pub async fn send_clipboard(&mut self, text: &str) -> Result<(), SessionError> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Err(SessionError::Protocol(
                "the clipboard text is larger than 1 MiB",
            ));
        }
        self.channel
            .send(&Message::ClipboardSet {
                text: text.to_owned(),
            })
            .await?;
        Ok(())
    }

    /// Client side: the text on the host's clipboard. Needs `files`. `None` means the host
    /// refused or has no text on its clipboard; the reason arrives as a notice.
    pub async fn get_clipboard(&mut self) -> Result<Option<String>, SessionError> {
        self.channel.send(&Message::ClipboardGet).await?;
        loop {
            match timeout(SIGNAL_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::ClipboardData { text } => return Ok(text),
                Message::Permissions { granted } => self.permissions = granted.into(),
                Message::Notice { text } => self.push_notice(text),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
            }
        }
    }

    /// Reads whatever the host sent before hanging up, looking for its reason.
    async fn read_goodbye(&mut self) -> Option<String> {
        loop {
            match timeout(Duration::from_millis(250), self.channel.recv()).await {
                Ok(Ok(Message::Bye { reason })) => return Some(reason),
                Ok(Ok(_)) => continue,
                _ => return None,
            }
        }
    }

    pub async fn bye(&mut self, reason: &str) -> Result<(), SessionError> {
        self.channel
            .send(&Message::Bye {
                reason: reason.into(),
            })
            .await?;
        Ok(())
    }

    /// Client side: asks the host for its screen. `offer` is a WebRTC session description;
    /// the return value is the host's answer. Fails with [`SessionError::Signal`] if the
    /// host refuses, for example because this device lacks `view`.
    pub async fn request_video(&mut self, offer: String) -> Result<String, SessionError> {
        self.channel
            .send(&Message::Signal {
                kind: SignalKind::Offer,
                payload: offer,
            })
            .await?;
        loop {
            match timeout(SIGNAL_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::Signal {
                    kind: SignalKind::Answer,
                    payload,
                } => return Ok(payload),
                Message::Signal {
                    kind: SignalKind::Error,
                    payload,
                } => return Err(SessionError::Signal(payload)),
                Message::Permissions { granted } => self.permissions = granted.into(),
                Message::Notice { text } => self.push_notice(text),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
            }
        }
    }

    /// Host side. Re-reads this device's permissions from the trust store. If the device was
    /// unpaired, tells it so and returns `Revoked`. If the grant changed, updates the session
    /// and tells the client. Call before acting on every request from the peer.
    pub async fn refresh_permissions(&mut self, host: &HostConfig) -> Result<(), SessionError> {
        apply_permissions(
            host,
            &self.peer.public_key,
            &mut self.permissions,
            &mut self.channel.sender,
        )
        .await
    }

    /// Host side. Refreshes, then requires `needed`. This is the gate every permissioned
    /// action must pass through.
    pub async fn authorize(
        &mut self,
        host: &HostConfig,
        needed: Permission,
    ) -> Result<(), SessionError> {
        self.refresh_permissions(host).await?;
        self.permissions.require(needed)?;
        Ok(())
    }
}

/// Shared by `refresh_permissions` and the serve loop.
async fn apply_permissions<W: AsyncWrite + Unpin>(
    host: &HostConfig,
    key: &[u8; 32],
    current: &mut PermissionSet,
    sender: &mut SecureSender<W>,
) -> Result<(), SessionError> {
    match host.permissions_for(key)? {
        None => {
            let _ = sender
                .send(&Message::Bye {
                    reason: "access revoked".into(),
                })
                .await;
            Err(SessionError::Revoked)
        }
        Some(now) if now != *current => {
            *current = now;
            sender
                .send(&Message::Permissions {
                    granted: now.to_vec(),
                })
                .await?;
            Ok(())
        }
        Some(_) => Ok(()),
    }
}

impl<S> Session<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Host side loop. Answers pings, video offers and input until the peer leaves, goes idle
    /// or is revoked. Every `PERMISSION_POLL` it re-checks the device against the trust file,
    /// so a revoked grant takes effect, and video and input stop, even if the peer says
    /// nothing.
    pub async fn serve(self, host: &HostConfig) -> Result<(), SessionError> {
        let Session {
            channel,
            peer,
            mut permissions,
            media_guard,
            ..
        } = self;
        let SecureChannel {
            mut sender,
            receiver,
            ..
        } = channel;

        // Reading happens in its own task so the loop below can also wake on a timer.
        // (A half-read frame must never be cancelled, so the read is not part of the select.)
        let (tx, mut rx) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            let mut receiver = receiver;
            loop {
                let msg = receiver.recv().await;
                let done = msg.is_err();
                if tx.send(msg).await.is_err() || done {
                    break;
                }
            }
        });

        let mut state = HostState::new(media_guard);
        let result = serve_loop(
            host,
            &peer,
            &mut permissions,
            &mut sender,
            &mut rx,
            &mut state,
        )
        .await;
        reader.abort();
        // Dropping the state ends video and releases any held keys and buttons.
        drop(state);
        result
    }
}

/// Everything the host holds open for one connected device.
struct HostState {
    media: Option<Box<dyn Send>>,
    /// One running stream per display, for viewers that show each screen separately.
    displays: HashMap<u32, Box<dyn Send>>,
    display_cache: Option<(Instant, Vec<DisplayDesc>)>,
    input: Option<Box<dyn InputSink>>,
    /// Notices already sent, so each reason is told once until permissions change.
    notified: HashSet<&'static str>,
    window_start: Instant,
    window_count: u32,
    input_retry_at: Option<Instant>,
    last_wake: Option<Instant>,
}

impl HostState {
    fn new(media: Option<Box<dyn Send>>) -> Self {
        Self {
            media,
            displays: HashMap::new(),
            display_cache: None,
            input: None,
            notified: HashSet::new(),
            window_start: Instant::now(),
            window_count: 0,
            input_retry_at: None,
            last_wake: None,
        }
    }

    /// Closes whatever the current permissions no longer allow.
    fn enforce(&mut self, p: PermissionSet) {
        if !p.contains(Permission::View) {
            self.media = None;
            self.displays.clear();
        }
        if !p.contains(Permission::Input) && !p.contains(Permission::Media) {
            self.input = None;
        }
    }

    /// True if a droppable event fits in the current one second budget.
    fn within_rate(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.window_count = 0;
        }
        self.window_count += 1;
        self.window_count <= MAX_DROPPABLE_INPUT_PER_SEC
    }
}

/// Re-reads permissions, and if they changed, forgets which notices were sent and closes
/// whatever is no longer allowed.
async fn sync_permissions<W: AsyncWrite + Unpin>(
    host: &HostConfig,
    peer: &PeerInfo,
    permissions: &mut PermissionSet,
    sender: &mut SecureSender<W>,
    state: &mut HostState,
) -> Result<(), SessionError> {
    let before = *permissions;
    apply_permissions(host, &peer.public_key, permissions, sender).await?;
    if *permissions != before {
        state.notified.clear();
    }
    state.enforce(*permissions);
    Ok(())
}

async fn serve_loop<W: AsyncWrite + Unpin>(
    host: &HostConfig,
    peer: &PeerInfo,
    permissions: &mut PermissionSet,
    sender: &mut SecureSender<W>,
    rx: &mut mpsc::Receiver<Result<Message, ChannelError>>,
    state: &mut HostState,
) -> Result<(), SessionError> {
    let mut tick = interval(PERMISSION_POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_heard = Instant::now();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                sync_permissions(host, peer, permissions, sender, state).await?;
                if last_heard.elapsed() > IDLE_TIMEOUT {
                    return Err(SessionError::Timeout);
                }
            }
            incoming = rx.recv() => {
                let msg = match incoming {
                    None | Some(Err(ChannelError::Closed)) => return Ok(()),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(m)) => m,
                };
                last_heard = Instant::now();
                sync_permissions(host, peer, permissions, sender, state).await?;
                match msg {
                    Message::Ping { nonce } => sender.send(&Message::Pong { nonce }).await?,
                    Message::Bye { .. } => return Ok(()),
                    Message::Signal { kind: SignalKind::Offer, payload } => {
                        // A new offer replaces any running video.
                        state.media = None;
                        let reply = match offer_reply(host, peer, *permissions, payload).await {
                            Ok((sdp, guard)) => {
                                state.media = Some(guard);
                                Message::Signal { kind: SignalKind::Answer, payload: sdp }
                            }
                            Err(reason) => Message::Signal { kind: SignalKind::Error, payload: reason },
                        };
                        sender.send(&reply).await?;
                    }
                    Message::ListDisplays => {
                        let list = match permissions.require(Permission::View) {
                            Ok(()) => host.media.as_ref().map(|m| m.displays()).unwrap_or_default(),
                            Err(_) => {
                                let text = "This device is not allowed to use `view` on the host.".to_owned();
                                sender.send(&Message::Notice { text }).await?;
                                Vec::new()
                            }
                        };
                        sender.send(&Message::Displays { list }).await?;
                    }
                    Message::DisplaySignal { display, kind: SignalKind::Offer, payload } => {
                        // A new offer for a display replaces that display's stream only.
                        state.displays.remove(&display);
                        let reply = match display_offer_reply(host, peer, *permissions, display, payload).await {
                            Ok((sdp, guard)) => {
                                state.displays.insert(display, guard);
                                Message::DisplaySignal { display, kind: SignalKind::Answer, payload: sdp }
                            }
                            Err(reason) => Message::DisplaySignal { display, kind: SignalKind::Error, payload: reason },
                        };
                        sender.send(&reply).await?;
                    }
                    Message::Input(event) => {
                        handle_input(host, peer, *permissions, sender, state, event).await?;
                    }
                    Message::Wake { target } => {
                        let text = handle_wake(host, *permissions, state, &target);
                        sender.send(&Message::Notice { text }).await?;
                    }
                    Message::ClipboardGet => {
                        let (notice, text) = handle_clipboard_get(host, *permissions).await;
                        if let Some(text) = notice {
                            sender.send(&Message::Notice { text }).await?;
                        }
                        sender.send(&Message::ClipboardData { text }).await?;
                    }
                    Message::ClipboardSet { text } => {
                        let text = handle_clipboard_set(host, *permissions, text).await;
                        sender.send(&Message::Notice { text }).await?;
                    }
                    other => tracing::debug!(?other, "ignoring unexpected message"),
                }
            }
        }
    }
}

/// Tells the client something once. Later identical reasons stay quiet until permissions
/// change, so a stream of mouse moves cannot turn into a stream of notices.
async fn notify_once<W: AsyncWrite + Unpin>(
    sender: &mut SecureSender<W>,
    state: &mut HostState,
    reason: &'static str,
    text: String,
) -> Result<(), SessionError> {
    if state.notified.insert(reason) {
        sender.send(&Message::Notice { text }).await?;
    }
    Ok(())
}

/// Every wake request gets an answer, because the person pressed a button and is waiting.
fn handle_wake(
    host: &HostConfig,
    permissions: PermissionSet,
    state: &mut HostState,
    target: &str,
) -> String {
    if permissions.require(Permission::Power).is_err() {
        return "This device is not allowed to use `power` on the host.".to_owned();
    }
    let Some(provider) = &host.wake else {
        return "This host is not set up to wake other computers.".to_owned();
    };
    if state
        .last_wake
        .is_some_and(|at| at.elapsed() < WAKE_COOLDOWN)
    {
        return "Wait a moment before sending another wake signal.".to_owned();
    }
    state.last_wake = Some(Instant::now());
    provider.wake(target).unwrap_or_else(|reason| reason)
}

/// The clipboard provider, if this device may use it. The error is what to tell the person.
fn clipboard_for(
    host: &HostConfig,
    permissions: PermissionSet,
) -> Result<Arc<dyn ClipboardProvider>, String> {
    if permissions.require(Permission::Files).is_err() {
        return Err("This device is not allowed to use `files` on the host.".to_owned());
    }
    host.clipboard
        .clone()
        .ok_or_else(|| "Clipboard sharing is turned off on the host.".to_owned())
}

/// Every clipboard request gets an answer, because the person pressed a button and is
/// waiting. Returns a notice to show (if any) and the text (if any).
async fn handle_clipboard_get(
    host: &HostConfig,
    permissions: PermissionSet,
) -> (Option<String>, Option<String>) {
    let provider = match clipboard_for(host, permissions) {
        Ok(p) => p,
        Err(notice) => return (Some(notice), None),
    };
    match tokio::task::spawn_blocking(move || provider.read()).await {
        Ok(Ok(Some(text))) if text.len() > MAX_CLIPBOARD_BYTES => (
            Some("The host's clipboard text is larger than 1 MiB, so it was not sent.".to_owned()),
            None,
        ),
        Ok(Ok(Some(text))) if !text.is_empty() => (None, Some(text)),
        Ok(Ok(_)) => (
            Some("The host's clipboard has no text on it.".to_owned()),
            None,
        ),
        Ok(Err(reason)) => (Some(reason), None),
        Err(_) => (
            Some("The host could not read its clipboard.".to_owned()),
            None,
        ),
    }
}

async fn handle_clipboard_set(
    host: &HostConfig,
    permissions: PermissionSet,
    text: String,
) -> String {
    let provider = match clipboard_for(host, permissions) {
        Ok(p) => p,
        Err(notice) => return notice,
    };
    if text.len() > MAX_CLIPBOARD_BYTES {
        return "That text is larger than 1 MiB, so the host's clipboard was not changed."
            .to_owned();
    }
    match tokio::task::spawn_blocking(move || provider.write(&text)).await {
        Ok(Ok(())) => "Copied to the host's clipboard.".to_owned(),
        Ok(Err(reason)) => reason,
        Err(_) => "The host could not write to its clipboard.".to_owned(),
    }
}

async fn handle_input<W: AsyncWrite + Unpin>(
    host: &HostConfig,
    peer: &PeerInfo,
    permissions: PermissionSet,
    sender: &mut SecureSender<W>,
    state: &mut HostState,
    event: InputEvent,
) -> Result<(), SessionError> {
    let needed = event.required_permission();
    if !permissions.contains(needed) {
        let text = format!("This device is not allowed to use `{needed}` on the host.");
        return notify_once(sender, state, "denied", text).await;
    }
    let Some(provider) = &host.input else {
        let text = "Remote control is turned off on the host.".to_owned();
        return notify_once(sender, state, "off", text).await;
    };
    let event = match event {
        InputEvent::MouseMoveOn { display, x, y } => {
            match display_point(host, state, display, x, y) {
                Some(at) => at,
                None => {
                    let text = format!("The host has no display {display} to point at.");
                    return notify_once(sender, state, "display", text).await;
                }
            }
        }
        other => other,
    };
    if event.is_droppable() && !state.within_rate() {
        return Ok(());
    }
    if state.input.is_none() {
        if state.input_retry_at.is_some_and(|at| Instant::now() < at) {
            return Ok(());
        }
        match provider.open(peer) {
            Ok(sink) => {
                state.input = Some(sink);
                state.input_retry_at = None;
            }
            Err(reason) => {
                state.input_retry_at = Some(Instant::now() + INPUT_RETRY);
                return notify_once(sender, state, "open", reason).await;
            }
        }
    }
    if let Some(sink) = state.input.as_mut() {
        if let Err(reason) = sink.apply(event) {
            return notify_once(sender, state, "apply", reason).await;
        }
    }
    Ok(())
}

/// Where a pointer position on one display lands in the host's combined desktop.
fn display_point(
    host: &HostConfig,
    state: &mut HostState,
    display: u32,
    x: u16,
    y: u16,
) -> Option<InputEvent> {
    let stale = match &state.display_cache {
        Some((at, _)) => at.elapsed() > DISPLAY_CACHE_TTL,
        None => true,
    };
    if stale {
        let list = host
            .media
            .as_ref()
            .map(|m| m.displays())
            .unwrap_or_default();
        state.display_cache = Some((Instant::now(), list));
    }
    let (_, list) = state.display_cache.as_ref()?;
    let d = list.iter().find(|d| d.id == display)?;
    let at = |origin: i32, size: u32, frac: u16| {
        let span = f64::from(size.saturating_sub(1));
        origin + (f64::from(frac) / 65535.0 * span).round() as i32
    };
    Some(InputEvent::MouseMoveAt {
        x: at(d.x, d.width, x),
        y: at(d.y, d.height, y),
    })
}

async fn display_offer_reply(
    host: &HostConfig,
    peer: &PeerInfo,
    permissions: PermissionSet,
    display: u32,
    offer: String,
) -> Result<(String, Box<dyn Send>), String> {
    permissions
        .require(Permission::View)
        .map_err(|e| e.to_string())?;
    let Some(media) = &host.media else {
        return Err("this device is not sharing its screen".into());
    };
    let answer = media.answer_display(peer, display, offer).await?;
    Ok((answer.sdp, answer.guard))
}

async fn offer_reply(
    host: &HostConfig,
    peer: &PeerInfo,
    permissions: PermissionSet,
    offer: String,
) -> Result<(String, Box<dyn Send>), String> {
    permissions
        .require(Permission::View)
        .map_err(|e| e.to_string())?;
    let Some(media) = &host.media else {
        return Err("this device is not sharing its screen".into());
    };
    let answer = media.answer(peer, offer).await?;
    Ok((answer.sdp, answer.guard))
}
