//! The seam between the control session and whatever moves the real mouse and keyboard.
//! Core stays free of platform code; `tether-input` implements these traits.

use crate::session::PeerInfo;
use tether_proto::InputEvent;

/// Applies input on behalf of one connected device. Dropping it must release every key and
/// button it is still holding down, so a dropped connection never leaves a key stuck.
pub trait InputSink: Send {
    /// The error string is shown to the person at the controlling device, so keep it readable.
    fn apply(&mut self, event: InputEvent) -> Result<(), String>;
}

pub trait InputProvider: Send + Sync {
    /// Opens a sink for `peer`. Fails, with a readable reason, if this computer cannot be
    /// controlled right now (for example macOS has not granted Accessibility access).
    fn open(&self, peer: &PeerInfo) -> Result<Box<dyn InputSink>, String>;
}
