//! The client side of Tether for other languages, through UniFFI. Swift calls this on the
//! iPhone; the same interface is exercised from Python in tests.
//!
//! This is only the controller half: pair with a host, connect, keep the session alive, send
//! mouse and keyboard input, read the host's notices. Hosting stays on the desktop.

uniffi::setup_scaffolding!();

use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tether_core::{
    identity::Identity,
    link::{with_default_port, PairingLink},
    paths::Paths,
    proto::{InputEvent, Key, MouseButton},
    session::{connect, ClientMode, Outcome, Session},
    trust::{TrustStore, TrustedPeer},
};
use tokio::{net::TcpStream, sync::Mutex};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TetherError {
    /// `message` is written for the person using the app.
    #[error("{message}")]
    Failed { message: String },
}

fn fail(e: impl std::fmt::Display) -> TetherError {
    TetherError::Failed {
        message: e.to_string(),
    }
}

/// A paired device, or the host on the other end of a connection.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PeerSummary {
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    /// On a connection: what the host allows this device to do (`view`, `input`, `media`,
    /// `power`, `files`).
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PairingInfo {
    pub address: String,
    pub code: String,
    pub name: Option<String>,
}

/// One thing the person did on the phone.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum InputAction {
    /// Absolute pointer position, 0 to 1 across and down the host's display.
    Move { x: f64, y: f64 },
    /// Relative pointer movement in the host's pixels. What a trackpad sends.
    Delta { dx: i32, dy: i32 },
    /// `left`, `right`, `middle`, `back` or `forward`.
    Button { button: String, down: bool },
    /// Lines. Positive scrolls down or right.
    Scroll { dx: i32, dy: i32 },
    /// A DOM `KeyboardEvent.code` such as `Enter`, `Backspace` or `ArrowUp`.
    Key { code: String, down: bool },
    /// Types this text exactly, whatever the host's keyboard layout.
    Text { text: String },
}

fn to_event(action: InputAction) -> Result<InputEvent, TetherError> {
    let frac = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let lines = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    Ok(match action {
        InputAction::Move { x, y } => InputEvent::MouseMove {
            x: frac(x),
            y: frac(y),
        },
        InputAction::Delta { dx, dy } => InputEvent::MouseDelta {
            dx: lines(dx),
            dy: lines(dy),
        },
        InputAction::Button { button, down } => InputEvent::MouseButton {
            button: MouseButton::from_name(&button)
                .ok_or_else(|| fail(format!("unknown mouse button {button:?}")))?,
            down,
        },
        InputAction::Scroll { dx, dy } => InputEvent::Scroll {
            dx: lines(dx),
            dy: lines(dy),
        },
        InputAction::Key { code, down } => InputEvent::Key {
            key: Key::from_dom_code(&code)
                .ok_or_else(|| fail(format!("unsupported key {code:?}")))?,
            down,
        },
        InputAction::Text { text } => InputEvent::Text { text },
    })
}

/// Reads a scanned QR code or pasted link.
#[uniffi::export]
pub fn parse_pairing_link(link: String) -> Result<PairingInfo, TetherError> {
    let l = PairingLink::parse(&link).map_err(fail)?;
    Ok(PairingInfo {
        address: l.address,
        code: l.code,
        name: l.name,
    })
}

#[uniffi::export]
pub fn make_pairing_link(
    address: String,
    code: String,
    name: Option<String>,
) -> Result<String, TetherError> {
    Ok(PairingLink::new(&address, &code, name.as_deref())
        .map_err(fail)?
        .to_string())
}

/// This phone's identity and the hosts it has paired with.
#[derive(uniffi::Object)]
pub struct Device {
    paths: Paths,
    identity: Arc<Identity>,
    name: String,
}

#[uniffi::export(async_runtime = "tokio")]
impl Device {
    /// `home` is a directory this app owns (on iOS, inside Application Support). The device
    /// key lives there, so it must be kept across launches.
    #[uniffi::constructor]
    pub fn new(home: String, name: String) -> Result<Arc<Self>, TetherError> {
        let paths = Paths::resolve(Some(PathBuf::from(home))).map_err(fail)?;
        let identity = Identity::load_or_create(&paths.identity_file()).map_err(fail)?;
        Ok(Arc::new(Self {
            paths,
            identity: Arc::new(identity),
            name,
        }))
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint()
    }

    /// Hosts this phone has paired with.
    pub fn peers(&self) -> Result<Vec<PeerSummary>, TetherError> {
        Ok(self.store()?.peers().iter().map(summary).collect())
    }

    /// Forgets a host, by name or fingerprint prefix. Returns how many were removed.
    pub fn remove(&self, query: String) -> Result<u32, TetherError> {
        Ok(self.store()?.remove(&query).map_err(fail)? as u32)
    }

    /// Pairs with a host that is showing `code`. `address` is `host` or `host:port`.
    pub async fn pair(&self, address: String, code: String) -> Result<PeerSummary, TetherError> {
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
        .map_err(fail)?
        {
            Outcome::Paired(peer) => Ok(PeerSummary {
                name: peer.name.clone(),
                platform: peer.platform.to_string(),
                fingerprint: peer.fingerprint(),
                permissions: vec![],
            }),
            Outcome::Authenticated(_) => Err(fail("unexpected session while pairing")),
        }
    }

    /// Opens a session with a host this phone has paired with.
    pub async fn connect(&self, address: String) -> Result<Arc<Connection>, TetherError> {
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
        .map_err(fail)?
        {
            Outcome::Authenticated(session) => Ok(Connection::new(session)),
            Outcome::Paired(_) => Err(fail("unexpected pairing result")),
        }
    }
}

impl Device {
    fn store(&self) -> Result<TrustStore, TetherError> {
        TrustStore::load(self.paths.trust_file()).map_err(fail)
    }
}

fn summary(p: &TrustedPeer) -> PeerSummary {
    PeerSummary {
        name: p.name.clone(),
        platform: p.platform.to_string(),
        fingerprint: p.fingerprint(),
        permissions: p
            .permissions
            .to_vec()
            .into_iter()
            .map(|p| p.name().to_owned())
            .collect(),
    }
}

async fn dial(address: &str) -> Result<TcpStream, TetherError> {
    let target = with_default_port(address);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&target))
        .await
        .map_err(|_| fail(format!("timed out connecting to {target}")))?
        .map_err(|e| fail(format!("could not connect to {target}: {e}")))?;
    stream.set_nodelay(true).map_err(fail)?;
    Ok(stream)
}

/// An open session with a host.
#[derive(uniffi::Object)]
pub struct Connection {
    session: Mutex<Option<Session<TcpStream>>>,
    peer: PeerSummary,
    permissions: StdMutex<Vec<String>>,
}

impl Connection {
    fn new(session: Session<TcpStream>) -> Arc<Self> {
        let permissions: Vec<String> = session
            .permissions
            .to_vec()
            .into_iter()
            .map(|p| p.name().to_owned())
            .collect();
        let peer = PeerSummary {
            name: session.peer.name.clone(),
            platform: session.peer.platform.to_string(),
            fingerprint: session.peer.fingerprint(),
            permissions: permissions.clone(),
        };
        Arc::new(Self {
            session: Mutex::new(Some(session)),
            peer,
            permissions: StdMutex::new(permissions),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Connection {
    /// The host, with the permissions it gave this phone when the session opened.
    pub fn peer(&self) -> PeerSummary {
        self.peer.clone()
    }

    /// What the host currently allows this phone to do. Refreshed by `ping`.
    pub fn permissions(&self) -> Vec<String> {
        self.permissions.lock().expect("lock").clone()
    }

    /// Round trip in milliseconds. Call every few seconds: it keeps the host from dropping an
    /// idle session, and it is how permission changes and notices reach the phone. If it
    /// fails the session is over.
    pub async fn ping(&self, nonce: u64) -> Result<f64, TetherError> {
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| fail("connection is closed"))?;
        let result = session.ping(nonce).await;
        *self.permissions.lock().expect("lock") = session
            .permissions
            .to_vec()
            .into_iter()
            .map(|p| p.name().to_owned())
            .collect();
        match result {
            Ok(rtt) => Ok(rtt.as_secs_f64() * 1000.0),
            Err(e) => {
                *guard = None;
                Err(fail(e))
            }
        }
    }

    /// Sends one mouse or keyboard action. If the host refuses it, the reason shows up in
    /// `take_notices` after the next ping.
    pub async fn send_input(&self, action: InputAction) -> Result<(), TetherError> {
        let event = to_event(action)?;
        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| fail("connection is closed"))?;
        if let Err(e) = session.send_input(event).await {
            *guard = None;
            return Err(fail(e));
        }
        Ok(())
    }

    /// Messages from the host for the person to read, such as "remote control is turned off
    /// on the host". They arrive during `ping`.
    pub async fn take_notices(&self) -> Vec<String> {
        match self.session.lock().await.as_mut() {
            Some(session) => session.take_notices(),
            None => Vec::new(),
        }
    }

    pub async fn close(&self) -> Result<(), TetherError> {
        if let Some(mut session) = self.session.lock().await.take() {
            let _ = session.bye("closed").await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_convert_and_clamp() {
        assert_eq!(
            to_event(InputAction::Move { x: 2.0, y: -1.0 }).unwrap(),
            InputEvent::MouseMove { x: 65535, y: 0 }
        );
        assert_eq!(
            to_event(InputAction::Delta {
                dx: 100_000,
                dy: -100_000
            })
            .unwrap(),
            InputEvent::MouseDelta {
                dx: i16::MAX,
                dy: i16::MIN
            }
        );
        assert_eq!(
            to_event(InputAction::Key {
                code: "ArrowUp".into(),
                down: true
            })
            .unwrap(),
            InputEvent::Key {
                key: Key::ArrowUp,
                down: true
            }
        );
        assert!(to_event(InputAction::Key {
            code: "Nope".into(),
            down: true
        })
        .is_err());
        assert!(to_event(InputAction::Button {
            button: "side".into(),
            down: true
        })
        .is_err());
    }

    #[test]
    fn links_round_trip_through_the_exported_functions() {
        let link =
            make_pairing_link("10.0.0.2:7420".into(), "0123456789abcdef".into(), None).unwrap();
        let info = parse_pairing_link(link).unwrap();
        assert_eq!(info.address, "10.0.0.2:7420");
        assert_eq!(info.code, "0123-4567-89AB-CDEF");
        assert!(parse_pairing_link("nope".into()).is_err());
    }
}
