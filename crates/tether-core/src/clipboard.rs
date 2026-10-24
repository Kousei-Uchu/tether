//! The seam between the control session and the computer's clipboard. Core stays free of
//! platform code; `tether-clipboard` implements this trait.

/// Reads and writes the text on this computer's clipboard on behalf of a connected device.
/// The error strings are shown to the person at the other device, so keep them readable.
///
/// Calls may block (the OS clipboard can be slow or held by another program), so the session
/// runs them off the async runtime.
pub trait ClipboardProvider: Send + Sync {
    /// The text on the clipboard, or `None` if it holds no text (an image, say, or nothing).
    fn read(&self) -> Result<Option<String>, String>;
    /// Replaces the clipboard with `text`.
    fn write(&self, text: &str) -> Result<(), String>;
}
