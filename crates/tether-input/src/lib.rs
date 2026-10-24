//! Moves the real mouse and presses the real keys on this computer, for a device the host
//! has allowed to. Uses `enigo`: SendInput on Windows, CGEvent on macOS, XTEST on X11.
//!
//! Each controlling device gets its own thread that owns the injector. That keeps a
//! non-`Send` OS handle off the async runtime, keeps a slow OS call from stalling the
//! session, and gives one place that knows what is held down so it can release it all when
//! the device disconnects or loses its permission.

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key as EKey, Keyboard, Mouse, Settings};
use std::{
    collections::HashSet,
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    time::Duration,
};
use tether_core::{
    input::{InputProvider, InputSink},
    proto::{InputEvent, Key, MouseButton},
    session::PeerInfo,
};

/// Longest text a single `Text` event may type.
const MAX_TEXT: usize = 2000;
const QUEUE: usize = 256;

/// Creates a sink per controlling device.
#[derive(Default)]
pub struct EnigoInput;

impl InputProvider for EnigoInput {
    fn open(&self, peer: &PeerInfo) -> Result<Box<dyn InputSink>, String> {
        let (tx, rx) = sync_channel::<InputEvent>(QUEUE);
        let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);
        let last_error = Arc::new(Mutex::new(None));

        let thread_error = last_error.clone();
        std::thread::Builder::new()
            .name(format!("tether-input-{}", peer.name))
            .spawn(move || run(rx, ready_tx, thread_error))
            .map_err(|e| format!("could not start the input thread: {e}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Box::new(ThreadedSink { tx, last_error })),
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err("remote control did not start in time".into()),
        }
    }
}

struct ThreadedSink {
    tx: SyncSender<InputEvent>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl InputSink for ThreadedSink {
    fn apply(&mut self, event: InputEvent) -> Result<(), String> {
        // Report a problem from an earlier event, once.
        if let Some(reason) = self.last_error.lock().expect("lock").take() {
            return Err(reason);
        }
        if event.is_droppable() {
            // A full queue means the machine is behind. Pointer moves are replaceable.
            match self.tx.try_send(event) {
                Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
                Err(TrySendError::Disconnected(_)) => Err("remote control stopped".into()),
            }
        } else {
            // Presses and releases must arrive, so wait for room.
            self.tx
                .send(event)
                .map_err(|_| "remote control stopped".to_owned())
        }
    }
}
// Dropping the sink closes the channel. The thread finishes what is queued, releases
// anything still held down, and exits.

fn run(
    rx: Receiver<InputEvent>,
    ready: SyncSender<Result<(), String>>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => {
            let _ = ready.send(Ok(()));
            e
        }
        Err(e) => {
            let _ = ready.send(Err(start_error(&e.to_string())));
            return;
        }
    };

    let mut held_keys: HashSet<Key> = HashSet::new();
    let mut held_buttons: HashSet<MouseButton> = HashSet::new();

    while let Ok(event) = rx.recv() {
        // A panic in an OS call must not take the whole app down, or leave keys held.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply(&mut enigo, &mut held_keys, &mut held_buttons, event)
        }))
        .unwrap_or_else(|_| Err("the operating system rejected that input".to_owned()));
        if let Err(reason) = outcome {
            tracing::debug!("input failed: {reason}");
            *last_error.lock().expect("lock") = Some(reason);
        }
    }

    for key in held_keys {
        if let Some(k) = to_enigo(key) {
            let _ = enigo.key(k, Direction::Release);
        }
    }
    for button in held_buttons {
        let _ = enigo.button(to_button(button), Direction::Release);
    }
}

fn start_error(detail: &str) -> String {
    if cfg!(target_os = "macos") {
        "This Mac has not allowed remote control. Open System Settings > Privacy & Security > \
         Accessibility, switch on Tether (or Electron while developing), then try again."
            .to_owned()
    } else {
        format!("Could not start remote control: {detail}")
    }
}

fn apply(
    enigo: &mut Enigo,
    held_keys: &mut HashSet<Key>,
    held_buttons: &mut HashSet<MouseButton>,
    event: InputEvent,
) -> Result<(), String> {
    let e = |err: enigo::InputError| err.to_string();
    match event {
        InputEvent::MouseMove { x, y } => {
            let (w, h) = enigo.main_display().map_err(e)?;
            let px = i64::from(x) * i64::from((w - 1).max(0)) / 65535;
            let py = i64::from(y) * i64::from((h - 1).max(0)) / 65535;
            enigo
                .move_mouse(px as i32, py as i32, Coordinate::Abs)
                .map_err(e)
        }
        // A position in the combined desktop, already worked out by the session from the
        // display the viewer pointed at.
        InputEvent::MouseMoveAt { x, y } => enigo.move_mouse(x, y, Coordinate::Abs).map_err(e),
        InputEvent::MouseMoveOn { .. } => {
            Err("the pointer position was not resolved to a display".to_owned())
        }
        InputEvent::MouseDelta { dx, dy } => enigo
            .move_mouse(i32::from(dx), i32::from(dy), Coordinate::Rel)
            .map_err(e),
        InputEvent::MouseButton { button, down } => {
            let dir = direction(down);
            enigo.button(to_button(button), dir).map_err(e)?;
            if down {
                held_buttons.insert(button);
            } else {
                held_buttons.remove(&button);
            }
            Ok(())
        }
        InputEvent::Scroll { dx, dy } => {
            if dy != 0 {
                enigo.scroll(i32::from(dy), Axis::Vertical).map_err(e)?;
            }
            if dx != 0 {
                enigo.scroll(i32::from(dx), Axis::Horizontal).map_err(e)?;
            }
            Ok(())
        }
        InputEvent::Key { key, down } => {
            let Some(k) = to_enigo(key) else {
                return Err(format!("this computer cannot press {}", key.dom_code()));
            };
            enigo.key(k, direction(down)).map_err(e)?;
            if down {
                held_keys.insert(key);
            } else {
                held_keys.remove(&key);
            }
            Ok(())
        }
        InputEvent::Text { text } => {
            if text.chars().count() > MAX_TEXT {
                return Err("that text is too long to type".into());
            }
            enigo.text(&text).map_err(e)
        }
    }
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

fn to_button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
    }
}

/// Keys are physical positions on a US keyboard. Letters, digits and punctuation go through
/// as the character that key makes on a US layout, so on a different host layout the result
/// follows that layout's rules. Use `Text` events to type exact characters.
pub fn to_enigo(key: Key) -> Option<EKey> {
    // On macOS, enigo turns a character into a key code by asking the Text Input Source APIs
    // for the current layout. Those APIs must run on the main thread, and macOS aborts the
    // process (SIGABRT) when they do not. Our injector runs on its own thread, so on macOS
    // every printable key is sent as a fixed virtual key code instead, which asks nothing.
    #[cfg(target_os = "macos")]
    if let Some(code) = mac_keycode(key) {
        return Some(EKey::Other(u32::from(code)));
    }
    let code = key.dom_code();
    if let Some(letter) = code.strip_prefix("Key").filter(|r| r.len() == 1) {
        return letter
            .chars()
            .next()
            .map(|c| EKey::Unicode(c.to_ascii_lowercase()));
    }
    if let Some(digit) = code.strip_prefix("Digit").filter(|r| r.len() == 1) {
        return digit.chars().next().map(EKey::Unicode);
    }
    Some(match key {
        Key::F1 => EKey::F1,
        Key::F2 => EKey::F2,
        Key::F3 => EKey::F3,
        Key::F4 => EKey::F4,
        Key::F5 => EKey::F5,
        Key::F6 => EKey::F6,
        Key::F7 => EKey::F7,
        Key::F8 => EKey::F8,
        Key::F9 => EKey::F9,
        Key::F10 => EKey::F10,
        Key::F11 => EKey::F11,
        Key::F12 => EKey::F12,
        Key::Escape => EKey::Escape,
        Key::Tab => EKey::Tab,
        Key::CapsLock => EKey::CapsLock,
        Key::Space => EKey::Space,
        Key::Enter => EKey::Return,
        Key::Backspace => EKey::Backspace,
        Key::Delete => EKey::Delete,
        Key::Insert => return insert_key(),
        Key::Home => EKey::Home,
        Key::End => EKey::End,
        Key::PageUp => EKey::PageUp,
        Key::PageDown => EKey::PageDown,
        Key::ArrowUp => EKey::UpArrow,
        Key::ArrowDown => EKey::DownArrow,
        Key::ArrowLeft => EKey::LeftArrow,
        Key::ArrowRight => EKey::RightArrow,
        Key::ShiftLeft => EKey::LShift,
        Key::ShiftRight => EKey::RShift,
        Key::ControlLeft => EKey::LControl,
        Key::ControlRight => EKey::RControl,
        // enigo has no left and right Alt or Meta on every platform, so both sides share one.
        Key::AltLeft | Key::AltRight => EKey::Alt,
        Key::MetaLeft | Key::MetaRight => EKey::Meta,
        Key::Minus => EKey::Unicode('-'),
        Key::Equal => EKey::Unicode('='),
        Key::BracketLeft => EKey::Unicode('['),
        Key::BracketRight => EKey::Unicode(']'),
        Key::Backslash => EKey::Unicode('\\'),
        Key::Semicolon => EKey::Unicode(';'),
        Key::Quote => EKey::Unicode('\''),
        Key::Backquote => EKey::Unicode('`'),
        Key::Comma => EKey::Unicode(','),
        Key::Period => EKey::Unicode('.'),
        Key::Slash => EKey::Unicode('/'),
        Key::VolumeUp => EKey::VolumeUp,
        Key::VolumeDown => EKey::VolumeDown,
        Key::VolumeMute => EKey::VolumeMute,
        Key::PlayPause => EKey::MediaPlayPause,
        Key::NextTrack => EKey::MediaNextTrack,
        Key::PreviousTrack => EKey::MediaPrevTrack,
        // Letters and digits were handled above.
        _ => return None,
    })
}

/// The macOS virtual key code (ANSI layout, from Carbon's `Events.h`) for a printable key.
/// These name physical positions, which is what the protocol's keys are.
pub fn mac_keycode(key: Key) -> Option<u16> {
    let code = key.dom_code();
    if let Some(letter) = code.strip_prefix("Key").filter(|r| r.len() == 1) {
        return match letter.chars().next()? {
            'A' => Some(0x00),
            'S' => Some(0x01),
            'D' => Some(0x02),
            'F' => Some(0x03),
            'H' => Some(0x04),
            'G' => Some(0x05),
            'Z' => Some(0x06),
            'X' => Some(0x07),
            'C' => Some(0x08),
            'V' => Some(0x09),
            'B' => Some(0x0B),
            'Q' => Some(0x0C),
            'W' => Some(0x0D),
            'E' => Some(0x0E),
            'R' => Some(0x0F),
            'Y' => Some(0x10),
            'T' => Some(0x11),
            'O' => Some(0x1F),
            'U' => Some(0x20),
            'I' => Some(0x22),
            'P' => Some(0x23),
            'L' => Some(0x25),
            'J' => Some(0x26),
            'K' => Some(0x28),
            'N' => Some(0x2D),
            'M' => Some(0x2E),
            _ => None,
        };
    }
    if let Some(digit) = code.strip_prefix("Digit").filter(|r| r.len() == 1) {
        return match digit.chars().next()? {
            '1' => Some(0x12),
            '2' => Some(0x13),
            '3' => Some(0x14),
            '4' => Some(0x15),
            '5' => Some(0x17),
            '6' => Some(0x16),
            '7' => Some(0x1A),
            '8' => Some(0x1C),
            '9' => Some(0x19),
            '0' => Some(0x1D),
            _ => None,
        };
    }
    Some(match key {
        Key::Equal => 0x18,
        Key::Minus => 0x1B,
        Key::BracketRight => 0x1E,
        Key::BracketLeft => 0x21,
        Key::Quote => 0x27,
        Key::Semicolon => 0x29,
        Key::Backslash => 0x2A,
        Key::Comma => 0x2B,
        Key::Slash => 0x2C,
        Key::Period => 0x2F,
        Key::Backquote => 0x32,
        _ => return None,
    })
}

/// macOS keyboards have no Insert key, so enigo does not define it there.
fn insert_key() -> Option<EKey> {
    #[cfg(not(target_os = "macos"))]
    {
        Some(EKey::Insert)
    }
    #[cfg(target_os = "macos")]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_protocol_key_maps_except_insert_on_macos() {
        for key in Key::ALL {
            let mapped = to_enigo(*key);
            if cfg!(target_os = "macos") && *key == Key::Insert {
                assert!(mapped.is_none());
            } else {
                assert!(mapped.is_some(), "{} has no mapping", key.dom_code());
            }
        }
    }

    #[test]
    fn mac_keycodes_cover_every_printable_key_without_repeats() {
        let mut seen = std::collections::HashSet::new();
        let mut printable = 0;
        for key in Key::ALL {
            let code = key.dom_code();
            let is_printable = code.starts_with("Key")
                || code.starts_with("Digit")
                || matches!(
                    code,
                    "Minus"
                        | "Equal"
                        | "BracketLeft"
                        | "BracketRight"
                        | "Backslash"
                        | "Semicolon"
                        | "Quote"
                        | "Backquote"
                        | "Comma"
                        | "Period"
                        | "Slash"
                );
            if is_printable {
                printable += 1;
                let k = mac_keycode(*key).unwrap_or_else(|| panic!("{code} has no mac code"));
                assert!(seen.insert(k), "{code} reuses code {k:#x}");
            } else {
                assert!(mac_keycode(*key).is_none(), "{code} should not be here");
            }
        }
        assert_eq!(printable, 26 + 10 + 11);
    }

    #[test]
    fn known_mac_keycodes() {
        assert_eq!(mac_keycode(Key::A), Some(0));
        assert_eq!(mac_keycode(Key::Z), Some(6));
        assert_eq!(mac_keycode(Key::D1), Some(0x12));
        assert_eq!(mac_keycode(Key::Slash), Some(0x2C));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn letters_and_digits_become_their_characters() {
        assert!(matches!(to_enigo(Key::A), Some(EKey::Unicode('a'))));
        assert!(matches!(to_enigo(Key::Z), Some(EKey::Unicode('z'))));
        assert!(matches!(to_enigo(Key::D7), Some(EKey::Unicode('7'))));
        assert!(matches!(to_enigo(Key::Slash), Some(EKey::Unicode('/'))));
    }

    /// Needs a display. On Linux run: xvfb-run -a cargo test -p tether-input -- --ignored
    #[test]
    #[ignore = "needs a display to inject into"]
    fn moves_the_real_pointer() {
        use tether_core::proto::Platform;
        let peer = PeerInfo {
            name: "t".into(),
            platform: Platform::Linux,
            public_key: [0; 32],
        };
        let mut sink = EnigoInput.open(&peer).expect("open injector");
        let reader = Enigo::new(&Settings::default()).expect("reader");
        let (w, h) = reader.main_display().unwrap();

        sink.apply(InputEvent::MouseMove { x: 32768, y: 16384 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (x, y) = reader.location().unwrap();
        assert!((x - w / 2).abs() <= 2, "x {x} not near {}", w / 2);
        assert!((y - h / 4).abs() <= 2, "y {y} not near {}", h / 4);

        sink.apply(InputEvent::MouseMoveAt { x: 123, y: 45 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (ax, ay) = reader.location().unwrap();
        assert!(
            (ax - 123).abs() <= 2 && (ay - 45).abs() <= 2,
            "at {ax},{ay}"
        );
        sink.apply(InputEvent::MouseMove { x: 32768, y: 16384 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (x, y) = reader.location().unwrap();

        sink.apply(InputEvent::MouseDelta { dx: 10, dy: 20 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (x2, y2) = reader.location().unwrap();
        assert_eq!((x2 - x, y2 - y), (10, 20));

        sink.apply(InputEvent::Key {
            key: Key::ShiftLeft,
            down: true,
        })
        .unwrap();
        sink.apply(InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
        })
        .unwrap();
        drop(sink); // must release both without panicking
        std::thread::sleep(Duration::from_millis(300));
    }
}
