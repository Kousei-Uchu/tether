//! Reads and replaces the text on this computer's clipboard, for a device the host has
//! allowed to. Uses `arboard`: the Win32 clipboard on Windows, NSPasteboard on macOS, and X11
//! selections on Linux.
//!
//! One `arboard::Clipboard` is kept for the life of the provider. On X11 the program that
//! last copied something has to stay alive to serve it to whoever pastes, so dropping the
//! handle right after a write would make the text vanish.

use arboard::Error;
use std::sync::Mutex;
use tether_core::clipboard::ClipboardProvider;

#[derive(Default)]
pub struct SystemClipboard {
    handle: Mutex<Option<arboard::Clipboard>>,
}

impl SystemClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` with the clipboard handle, opening it first if needed. If `f` fails for any
    /// reason other than "no text there", the handle is dropped so the next call opens a
    /// fresh one rather than reusing something that may have gone stale.
    fn with<T>(
        &self,
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut slot = self.handle.lock().expect("clipboard lock");
        if slot.is_none() {
            // Failing to open the clipboard at all means there is nothing to talk to (no
            // desktop session, say). arboard reports that as an opaque "unknown error" on
            // some platforms, so give every such failure the same readable answer.
            let opened = arboard::Clipboard::new().map_err(|e| {
                tracing::debug!("could not open the clipboard: {e}");
                Error::ClipboardNotSupported
            })?;
            *slot = Some(opened);
        }
        let result = f(slot.as_mut().expect("just opened"));
        if matches!(&result, Err(e) if !matches!(e, Error::ContentNotAvailable)) {
            *slot = None;
        }
        result
    }
}

fn describe(e: Error) -> String {
    match e {
        Error::ContentNotAvailable => "The clipboard has no text on it.".to_owned(),
        Error::ClipboardOccupied => {
            "Another program is using the clipboard right now. Try again in a moment.".to_owned()
        }
        Error::ClipboardNotSupported => {
            "This computer's clipboard cannot be reached (is a desktop session running?)."
                .to_owned()
        }
        other => format!("The clipboard failed: {other}"),
    }
}

impl ClipboardProvider for SystemClipboard {
    fn read(&self) -> Result<Option<String>, String> {
        match self.with(|c| c.get_text()) {
            Ok(text) => Ok(Some(text)),
            // Empty, or holding something that is not text (an image): not an error.
            Err(Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    fn write(&self, text: &str) -> Result<(), String> {
        self.with(|c| c.set_text(text.to_owned())).map_err(describe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_sentences() {
        assert!(describe(Error::ClipboardOccupied).contains("Try again"));
        assert!(describe(Error::ClipboardNotSupported).contains("desktop session"));
        assert!(describe(Error::ContentNotAvailable).contains("no text"));
    }

    /// Needs a real clipboard, so it only runs on request: on Linux under
    /// `xvfb-run -a cargo test -p tether-clipboard -- --include-ignored`.
    #[test]
    #[ignore = "needs a desktop session"]
    fn writes_and_reads_back_through_the_system_clipboard() {
        let clip = SystemClipboard::new();
        clip.write("tether clipboard test ✓\nsecond line").unwrap();
        assert_eq!(
            clip.read().unwrap().as_deref(),
            Some("tether clipboard test ✓\nsecond line")
        );
        // A second provider, standing in for another program, sees the same text.
        let other = SystemClipboard::new();
        assert_eq!(
            other.read().unwrap().as_deref(),
            Some("tether clipboard test ✓\nsecond line")
        );
    }
}
