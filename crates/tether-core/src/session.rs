//! Authentication and pairing on top of the secure channel.
//!
//! Flow (see docs/SPEC.md section 5):
//!   Noise XX handshake -> Hello both ways -> either `Ready` (paired client) or a
//!   one-shot pairing exchange (`PairRequest` / `PairAccept`).

use crate::{
    channel::{ChannelError, Role, SecureChannel},
    identity::{fingerprint, Identity},
    pairing::{self, PairingOffer, ProofRole},
    trust::{TrustStore, TrustedPeer},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tether_proto::{Hello, Intent, Message, Platform, PROTOCOL_VERSION};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

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
    #[error("protocol version mismatch (ours {ours}, theirs {theirs})")]
    VersionMismatch { ours: u16, theirs: u16 },
    #[error("pairing failed: {0}")]
    PairingFailed(String),
    #[error("invalid pairing code; expected 16 letters and digits like XXXX-XXXX-XXXX-XXXX")]
    InvalidCode,
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    #[error("timed out")]
    Timeout,
    #[error("could not update the trust store: {0}")]
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
            let trusted = host.trust.lock().expect("trust lock").contains(&remote_key);
            if !trusted {
                ch.send(&Message::Reject {
                    reason: "this device is not paired".into(),
                })
                .await?;
                return Err(SessionError::PeerNotTrusted {
                    fingerprint: peer.fingerprint(),
                });
            }
            ch.send(&Message::Ready).await?;
            Ok(Outcome::Authenticated(Session { channel: ch, peer }))
        }
        Intent::Pair => {
            let Message::PairRequest { proof } = ch.recv().await? else {
                return Err(SessionError::Protocol("expected pair request"));
            };
            match consume_pairing(host, &proof, ch.handshake_hash()) {
                Ok(host_proof) => {
                    host.trust
                        .lock()
                        .expect("trust lock")
                        .add(TrustedPeer::new(
                            peer.name.clone(),
                            &remote_key,
                            peer.platform,
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
/// dropped on success, on expiry, and after too many bad guesses.
fn consume_pairing(
    host: &HostConfig,
    proof: &[u8; 32],
    handshake_hash: &[u8],
) -> Result<[u8; 32], String> {
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
    Ok(pairing::proof(
        offer.code(),
        ProofRole::Host,
        handshake_hash,
    ))
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
            let peer = PeerInfo {
                name: theirs.device_name,
                platform: theirs.platform,
                public_key: host_key,
            };
            Ok(Outcome::Authenticated(Session { channel: ch, peer }))
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
            trust.add(TrustedPeer::new(
                peer.name.clone(),
                &host_key,
                peer.platform,
            ))?;
            Ok(Outcome::Paired(peer))
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Round trip time of one ping.
    pub async fn ping(&mut self, nonce: u64) -> Result<Duration, SessionError> {
        let start = Instant::now();
        self.channel.send(&Message::Ping { nonce }).await?;
        loop {
            match timeout(AUTH_TIMEOUT, self.channel.recv())
                .await
                .map_err(|_| SessionError::Timeout)??
            {
                Message::Pong { nonce: n } if n == nonce => return Ok(start.elapsed()),
                Message::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => continue,
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

    /// Host side loop for step 1: answer pings until the peer leaves or goes idle.
    pub async fn serve(mut self) -> Result<(), SessionError> {
        loop {
            let msg = match timeout(IDLE_TIMEOUT, self.channel.recv()).await {
                Err(_) => return Err(SessionError::Timeout),
                Ok(Err(ChannelError::Closed)) => return Ok(()),
                Ok(Err(e)) => return Err(e.into()),
                Ok(Ok(m)) => m,
            };
            match msg {
                Message::Ping { nonce } => self.channel.send(&Message::Pong { nonce }).await?,
                Message::Bye { .. } => return Ok(()),
                other => tracing::debug!(?other, "ignoring unexpected message"),
            }
        }
    }
}
