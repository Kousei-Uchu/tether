//! Wire types shared by every Tether peer (host agent, desktop client, iPhone client).
//!
//! The encoding is `postcard`. Enum variant order is part of the wire format, so new
//! variants are only ever appended. See `docs/SPEC.md` section 5.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Bumped on any incompatible wire change. Peers must match exactly for now.
pub const PROTOCOL_VERSION: u16 = 7;
pub const DEFAULT_PORT: u16 = 7420;
/// Upper bound for one decoded message. Larger messages are rejected by the channel.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;
/// Longest clipboard text a host will accept or send, in bytes of UTF-8. Anything larger is
/// refused on both sides rather than truncated, so a paste never arrives cut short.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Bytes of file in one `FileData` message. Senders use this size; receivers refuse more.
pub const FILE_CHUNK_BYTES: usize = 32 * 1024;
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Mixed into the Noise handshake so a version mismatch fails the handshake itself.
pub const PROLOGUE: &[u8] = b"tether/7";

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

macro_rules! keys {
    ($($name:ident => $code:literal),* $(,)?) => {
        /// A physical key, named after the DOM `KeyboardEvent.code` of a US keyboard.
        /// Wire order is stable (append only).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Key {
            $($name),*
        }

        impl Key {
            pub const ALL: &'static [Key] = &[$(Key::$name),*];

            /// The DOM `KeyboardEvent.code` for this key.
            pub fn dom_code(self) -> &'static str {
                match self {
                    $(Key::$name => $code),*
                }
            }
        }
    };
}

keys! {
    A => "KeyA",
    B => "KeyB",
    C => "KeyC",
    D => "KeyD",
    E => "KeyE",
    F => "KeyF",
    G => "KeyG",
    H => "KeyH",
    I => "KeyI",
    J => "KeyJ",
    K => "KeyK",
    L => "KeyL",
    M => "KeyM",
    N => "KeyN",
    O => "KeyO",
    P => "KeyP",
    Q => "KeyQ",
    R => "KeyR",
    S => "KeyS",
    T => "KeyT",
    U => "KeyU",
    V => "KeyV",
    W => "KeyW",
    X => "KeyX",
    Y => "KeyY",
    Z => "KeyZ",
    D0 => "Digit0",
    D1 => "Digit1",
    D2 => "Digit2",
    D3 => "Digit3",
    D4 => "Digit4",
    D5 => "Digit5",
    D6 => "Digit6",
    D7 => "Digit7",
    D8 => "Digit8",
    D9 => "Digit9",
    F1 => "F1",
    F2 => "F2",
    F3 => "F3",
    F4 => "F4",
    F5 => "F5",
    F6 => "F6",
    F7 => "F7",
    F8 => "F8",
    F9 => "F9",
    F10 => "F10",
    F11 => "F11",
    F12 => "F12",
    Escape => "Escape",
    Tab => "Tab",
    CapsLock => "CapsLock",
    Space => "Space",
    Enter => "Enter",
    Backspace => "Backspace",
    Delete => "Delete",
    Insert => "Insert",
    Home => "Home",
    End => "End",
    PageUp => "PageUp",
    PageDown => "PageDown",
    ArrowUp => "ArrowUp",
    ArrowDown => "ArrowDown",
    ArrowLeft => "ArrowLeft",
    ArrowRight => "ArrowRight",
    ShiftLeft => "ShiftLeft",
    ShiftRight => "ShiftRight",
    ControlLeft => "ControlLeft",
    ControlRight => "ControlRight",
    AltLeft => "AltLeft",
    AltRight => "AltRight",
    MetaLeft => "MetaLeft",
    MetaRight => "MetaRight",
    Minus => "Minus",
    Equal => "Equal",
    BracketLeft => "BracketLeft",
    BracketRight => "BracketRight",
    Backslash => "Backslash",
    Semicolon => "Semicolon",
    Quote => "Quote",
    Backquote => "Backquote",
    Comma => "Comma",
    Period => "Period",
    Slash => "Slash",
    VolumeUp => "AudioVolumeUp",
    VolumeDown => "AudioVolumeDown",
    VolumeMute => "AudioVolumeMute",
    PlayPause => "MediaPlayPause",
    NextTrack => "MediaTrackNext",
    PreviousTrack => "MediaTrackPrevious",
}

impl Key {
    pub fn from_dom_code(code: &str) -> Option<Key> {
        Key::ALL.iter().copied().find(|k| k.dom_code() == code)
    }

    /// Media keys fall under `media`; everything else under `input`.
    pub fn is_media(self) -> bool {
        matches!(
            self,
            Key::VolumeUp
                | Key::VolumeDown
                | Key::VolumeMute
                | Key::PlayPause
                | Key::NextTrack
                | Key::PreviousTrack
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "left" => Self::Left,
            "right" => Self::Right,
            "middle" => Self::Middle,
            "back" => Self::Back,
            "forward" => Self::Forward,
            _ => return None,
        })
    }
}

/// One thing a controlling device does. Wire order is stable (append only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute pointer position as a fraction of the display, 0 to 65535 on each axis.
    MouseMove {
        x: u16,
        y: u16,
    },
    /// Relative pointer movement in pixels (a touch trackpad uses this).
    MouseDelta {
        dx: i16,
        dy: i16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Scroll in lines. Positive `dy` scrolls down, positive `dx` scrolls right.
    Scroll {
        dx: i16,
        dy: i16,
    },
    Key {
        key: Key,
        down: bool,
    },
    /// Types this text as is, whatever the keyboard layout. For on-screen keyboards.
    Text {
        text: String,
    },
    /// Absolute pointer position as a fraction (0 to 65535) of one display, for a viewer
    /// showing each display in its own window. The host turns it into `MouseMoveAt`.
    MouseMoveOn {
        display: u32,
        x: u16,
        y: u16,
    },
    /// Absolute pointer position in the host's combined desktop.
    MouseMoveAt {
        x: i32,
        y: i32,
    },
}

impl InputEvent {
    /// The permission a host must check before acting on this event.
    pub fn required_permission(&self) -> Permission {
        match self {
            InputEvent::Key { key, .. } if key.is_media() => Permission::Media,
            _ => Permission::Input,
        }
    }

    /// Pointer movement and scrolling can be dropped under load. Presses and releases
    /// never are, or a key could stay stuck down.
    pub fn is_droppable(&self) -> bool {
        matches!(
            self,
            InputEvent::MouseMove { .. }
                | InputEvent::MouseMoveOn { .. }
                | InputEvent::MouseMoveAt { .. }
                | InputEvent::MouseDelta { .. }
                | InputEvent::Scroll { .. }
        )
    }
}

/// One display on the host that can be streamed on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayDesc {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Top left corner in the host's combined desktop, in the same units as `width` and
    /// `height`. Negative if the display sits left of or above the primary one.
    pub x: i32,
    pub y: i32,
    pub primary: bool,
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
    /// Controlling device to host. Needs `input` (or `media` for media keys).
    Input(InputEvent),
    /// Host to client: something the person using the client should know, for example that
    /// the host has not allowed remote control. Not an error and never ends the session.
    Notice {
        text: String,
    },
    /// Client to host: broadcast a Wake-on-LAN packet for the computer called `target`.
    /// Needs `power`, and the host must have been given wake targets.
    Wake {
        target: String,
    },
    /// Client to host: which displays can be streamed one by one. Needs `view`.
    ListDisplays,
    /// Host to client: the answer to `ListDisplays`. Empty if the host shares no screen or
    /// this device lacks `view`.
    Displays {
        list: Vec<DisplayDesc>,
    },
    /// Like `Signal`, for one display. Each display has its own WebRTC connection, so a
    /// viewer can show every screen at once. Needs `view`.
    DisplaySignal {
        display: u32,
        kind: SignalKind,
        payload: String,
    },
    /// Client to host: asks for the text on the host's clipboard. Needs `files`, and the host
    /// must have turned clipboard sharing on. Answered with `ClipboardData`.
    ClipboardGet,
    /// Client to host: puts this text on the host's clipboard. Needs `files`, and the host
    /// must have turned clipboard sharing on. Answered with a `Notice`.
    ClipboardSet {
        text: String,
    },
    /// Host to client: the answer to `ClipboardGet`. `None` if the host refused or the
    /// clipboard holds no text; a `Notice` sent just before says why.
    ClipboardData {
        text: Option<String>,
    },
    /// Pairing, client to host: a hash of the client's random value, sent before the host
    /// reveals its own so neither side can steer the comparison string (see `sas`).
    VerifyCommit {
        commitment: [u8; 32],
    },
    /// Pairing, host to client: the host's random value.
    VerifyNonce {
        nonce: [u8; 32],
    },
    /// Pairing, client to host: the value behind the commitment.
    VerifyReveal {
        nonce: [u8; 32],
    },
    /// Pairing, both ways: whether the person confirmed the symbols match. A device is only
    /// stored if both said yes.
    VerifyDecision {
        accepted: bool,
    },
    /// Client to host: list a folder inside the shared folder (empty path is the folder
    /// itself). Needs `files` and a host that shares a folder. Answered with `FileEntries`.
    FileList {
        path: String,
    },
    /// Host to client: the answer to `FileList`. `error` says why `entries` is empty.
    FileEntries {
        path: String,
        entries: Vec<FileEntry>,
        error: Option<String>,
    },
    /// Client to host: send the file at `path`. Answered with `FileOk` then `FileData`
    /// messages and a `FileEnd`, or with `FileFail`.
    FileGet {
        id: u32,
        path: String,
    },
    /// Client to host: I am about to send `size` bytes to save as `name`. Answered with
    /// `FileOk` or `FileFail`; then the client sends `FileData` and `FileEnd`, and the host
    /// answers with `FileDone` or `FileFail`.
    FilePut {
        id: u32,
        name: String,
        size: u64,
    },
    /// The request with this `id` was accepted. For `FileGet`, `size` is the file's size.
    FileOk {
        id: u32,
        size: u64,
    },
    /// One piece of a file, at most `FILE_CHUNK_BYTES`, in order.
    FileData {
        id: u32,
        data: Vec<u8>,
    },
    /// The last message of a file: the SHA-256 of everything sent.
    FileEnd {
        id: u32,
        sha256: [u8; 32],
    },
    /// Host to client: an upload is saved. `name` is what it was saved as, which differs from
    /// the name asked for if that was taken.
    FileDone {
        id: u32,
        name: String,
    },
    /// Either way: refuse or stop the transfer with this `id`. The reason is for the person.
    FileFail {
        id: u32,
        reason: String,
    },
}

/// One line of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// Bytes; 0 for a folder.
    pub size: u64,
    pub dir: bool,
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
            Message::Input(InputEvent::MouseMove { x: 65535, y: 0 }),
            Message::Input(InputEvent::Text {
                text: "héllo".into(),
            }),
            Message::Notice { text: "hi".into() },
            Message::Wake {
                target: "desk".into(),
            },
            Message::ClipboardGet,
            Message::ClipboardSet {
                text: "paste me".into(),
            },
            Message::ClipboardData { text: None },
            Message::ClipboardData {
                text: Some("héllo".into()),
            },
        ];
        for m in msgs {
            let bytes = encode(&m).unwrap();
            assert_eq!(decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn verification_messages_are_appended_in_order() {
        let msgs = [
            (19, Message::VerifyCommit { commitment: [1; 32] }),
            (20, Message::VerifyNonce { nonce: [2; 32] }),
            (21, Message::VerifyReveal { nonce: [3; 32] }),
            (22, Message::VerifyDecision { accepted: true }),
        ];
        for (index, m) in msgs {
            let bytes = encode(&m).unwrap();
            assert_eq!(bytes[0], index);
            assert_eq!(decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn file_messages_are_appended_in_order() {
        let msgs = [
            (23, Message::FileList { path: "a".into() }),
            (
                24,
                Message::FileEntries {
                    path: String::new(),
                    entries: vec![FileEntry { name: "x".into(), size: 3, dir: false }],
                    error: None,
                },
            ),
            (25, Message::FileGet { id: 1, path: "a".into() }),
            (26, Message::FilePut { id: 1, name: "a".into(), size: 9 }),
            (27, Message::FileOk { id: 1, size: 9 }),
            (28, Message::FileData { id: 1, data: vec![1, 2, 3] }),
            (29, Message::FileEnd { id: 1, sha256: [4; 32] }),
            (30, Message::FileDone { id: 1, name: "a".into() }),
            (31, Message::FileFail { id: 1, reason: "no".into() }),
        ];
        for (index, m) in msgs {
            let bytes = encode(&m).unwrap();
            assert_eq!(bytes[0], index);
            assert_eq!(decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn a_full_file_chunk_fits_one_message() {
        let m = Message::FileData { id: u32::MAX, data: vec![0xAB; FILE_CHUNK_BYTES] };
        assert!(encode(&m).unwrap().len() < MAX_MESSAGE_LEN);
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
        assert_eq!(
            encode(&Message::Input(InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true
            }))
            .unwrap(),
            vec![10, 2, 1, 1]
        );
        assert_eq!(
            encode(&Message::Notice { text: "a".into() }).unwrap(),
            vec![11, 1, b'a']
        );
        assert_eq!(
            encode(&Message::Wake { target: "d".into() }).unwrap(),
            vec![12, 1, b'd']
        );
        assert_eq!(encode(&Message::ListDisplays).unwrap(), vec![13]);
        assert_eq!(
            encode(&Message::Displays { list: vec![] }).unwrap(),
            vec![14, 0]
        );
        assert_eq!(
            encode(&Message::DisplaySignal {
                display: 1,
                kind: SignalKind::Offer,
                payload: "x".into()
            })
            .unwrap(),
            vec![15, 1, 0, 1, b'x']
        );
        assert_eq!(encode(&Message::ClipboardGet).unwrap(), vec![16]);
        assert_eq!(
            encode(&Message::ClipboardSet { text: "a".into() }).unwrap(),
            vec![17, 1, b'a']
        );
        assert_eq!(
            encode(&Message::ClipboardData { text: None }).unwrap(),
            vec![18, 0]
        );
        assert_eq!(
            encode(&Message::ClipboardData {
                text: Some("a".into())
            })
            .unwrap(),
            vec![18, 1, 1, b'a']
        );
        assert_eq!(
            encode(&Message::Input(InputEvent::MouseMoveOn {
                display: 1,
                x: 2,
                y: 3
            }))
            .unwrap(),
            vec![10, 6, 1, 2, 3]
        );
        assert_eq!(
            encode(&Message::Input(InputEvent::MouseMoveAt { x: -1, y: 2 })).unwrap(),
            vec![10, 7, 1, 4]
        );
    }

    #[test]
    fn every_key_has_a_unique_dom_code_that_maps_back() {
        let mut seen = std::collections::HashSet::new();
        for k in Key::ALL {
            assert!(seen.insert(k.dom_code()), "duplicate {}", k.dom_code());
            assert_eq!(Key::from_dom_code(k.dom_code()), Some(*k));
        }
        assert_eq!(Key::from_dom_code("KeyA"), Some(Key::A));
        assert_eq!(Key::from_dom_code("Digit7"), Some(Key::D7));
        assert_eq!(Key::from_dom_code("NotAKey"), None);
    }

    #[test]
    fn permissions_for_events() {
        let key = |k| InputEvent::Key { key: k, down: true };
        assert_eq!(key(Key::A).required_permission(), Permission::Input);
        assert_eq!(key(Key::VolumeUp).required_permission(), Permission::Media);
        assert_eq!(
            InputEvent::Scroll { dx: 0, dy: 1 }.required_permission(),
            Permission::Input
        );
        assert!(InputEvent::MouseMove { x: 1, y: 1 }.is_droppable());
        assert!(!key(Key::A).is_droppable());
        assert!(!InputEvent::MouseButton {
            button: MouseButton::Left,
            down: false
        }
        .is_droppable());
    }
}
