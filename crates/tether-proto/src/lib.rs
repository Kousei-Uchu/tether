//! Wire types shared by every Tether peer (host agent, desktop client, iPhone client).
//!
//! The encoding is `postcard`. Enum variant order is part of the wire format, so new
//! variants are only ever appended. See `docs/SPEC.md` section 5.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Bumped on any incompatible wire change. Peers must match exactly for now.
pub const PROTOCOL_VERSION: u16 = 3;
pub const DEFAULT_PORT: u16 = 7420;
/// Upper bound for one decoded message. Larger messages are rejected by the channel.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Mixed into the Noise handshake so a version mismatch fails the handshake itself.
pub const PROLOGUE: &[u8] = b"tether/3";

pub type Proof = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Windows,
    MacOs,
    Ios,
    Linux,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "ios") {
            Platform::Ios
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Platform::Windows => "Windows",
            Platform::MacOs => "macOS",
            Platform::Ios => "iOS",
            Platform::Linux => "Linux",
            Platform::Other => "Other",
        })
    }
}

/// What a paired device is allowed to do on the host it connects to.
///
/// Wire order is stable (append only). JSON uses the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// See the screen and hear the audio.
    View,
    /// Mouse, keyboard and touch input.
    Input,
    /// Media keys, volume and brightness.
    Media,
    /// Lock, sleep, restart, shutdown and wake.
    Power,
    /// Clipboard sync and file transfer.
    Files,
}

impl Permission {
    pub const ALL: [Permission; 5] = [
        Permission::View,
        Permission::Input,
        Permission::Media,
        Permission::Power,
        Permission::Files,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Permission::View => "view",
            Permission::Input => "input",
            Permission::Media => "media",
            Permission::Power => "power",
            Permission::Files => "files",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why the client opened the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// Normal session with an already paired host.
    Connect,
    /// One-shot pairing. The connection closes after pairing completes.
    Pair,
}

/// What a [`Message::Signal`] carries. Wire order is stable (append only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalKind {
    /// Client to host: a WebRTC session description asking for the screen.
    Offer,
    /// Host to client: the matching answer.
    Answer,
    /// Host to client: the offer could not be served. The payload says why.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u16,
    pub device_name: String,
    pub platform: Platform,
    pub intent: Intent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    /// Client proves it knows the pairing code, bound to this Noise session.
    PairRequest {
        proof: Proof,
    },
    /// Host proves the same, which authenticates the host to the client.
    PairAccept {
        proof: Proof,
    },
    Reject {
        reason: String,
    },
    /// Sent by the host when a paired client is authenticated.
    Ready,
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Bye {
        reason: String,
    },
    /// Host tells the client what it may do. Sent right after `Ready`, and again whenever
    /// the grant changes while the session is open.
    Permissions {
        granted: Vec<Permission>,
    },
    /// WebRTC signaling carried over the authenticated channel. Needs the `view` permission.
    Signal {
        kind: SignalKind,
        payload: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("could not encode message: {0}")]
    Encode(postcard::Error),
    #[error("could not decode message: {0}")]
    Decode(postcard::Error),
    #[error("trailing bytes after message")]
    Trailing,
}

pub fn encode(msg: &Message) -> Result<Vec<u8>, CodecError> {
    postcard::to_stdvec(msg).map_err(CodecError::Encode)
}

pub fn decode(bytes: &[u8]) -> Result<Message, CodecError> {
    let (msg, rest) = postcard::take_from_bytes(bytes).map_err(CodecError::Decode)?;
    if !rest.is_empty() {
        return Err(CodecError::Trailing);
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hello() -> Message {
        Message::Hello(Hello {
            version: PROTOCOL_VERSION,
            device_name: "studio-mac".into(),
            platform: Platform::MacOs,
            intent: Intent::Pair,
        })
    }

    #[test]
    fn roundtrip_all_variants() {
        let msgs = vec![
            sample_hello(),
            Message::PairRequest { proof: [7; 32] },
            Message::PairAccept { proof: [9; 32] },
            Message::Reject {
                reason: "no".into(),
            },
            Message::Ready,
            Message::Ping { nonce: u64::MAX },
            Message::Pong { nonce: 0 },
            Message::Bye {
                reason: String::new(),
            },
            Message::Permissions {
                granted: Permission::ALL.to_vec(),
            },
            Message::Signal {
                kind: SignalKind::Error,
                payload: "nope".into(),
            },
        ];
        for m in msgs {
            let bytes = encode(&m).unwrap();
            assert_eq!(decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn rejects_trailing_and_garbage() {
        let mut bytes = encode(&Message::Ready).unwrap();
        bytes.push(0);
        assert!(matches!(decode(&bytes), Err(CodecError::Trailing)));
        assert!(decode(&[0xff, 0xff, 0xff]).is_err());
        assert!(decode(&[]).is_err());
    }

    /// Wire stability guard: variant order must never change. If this fails, you changed
    /// the protocol and need to bump PROTOCOL_VERSION.
    #[test]
    fn variant_indices_are_stable() {
        assert_eq!(encode(&Message::Ready).unwrap(), vec![4]);
        assert_eq!(encode(&Message::Ping { nonce: 1 }).unwrap(), vec![5, 1]);
        assert_eq!(encode(&Message::Pong { nonce: 1 }).unwrap(), vec![6, 1]);
        assert_eq!(
            encode(&Message::Permissions {
                granted: vec![Permission::Input, Permission::Files]
            })
            .unwrap(),
            vec![8, 2, 1, 4]
        );
        assert_eq!(
            encode(&Message::Signal {
                kind: SignalKind::Offer,
                payload: "x".into()
            })
            .unwrap(),
            vec![9, 0, 1, b'x']
        );
    }
}
