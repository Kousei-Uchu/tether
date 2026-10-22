//! The seam between the control session and whatever produces video. Core stays free of
//! codec and WebRTC dependencies; `tether-media` implements this trait.

use crate::session::PeerInfo;
use std::{future::Future, pin::Pin};

/// A started media session. Dropping the guard ends it (closes the peer connection, stops
/// capture), so the control session only has to hold on to it.
pub struct MediaAnswer {
    /// The WebRTC answer to send back to the client.
    pub sdp: String,
    pub guard: Box<dyn Send>,
}

pub type MediaFuture<'a> = Pin<Box<dyn Future<Output = Result<MediaAnswer, String>> + Send + 'a>>;

pub trait MediaProvider: Send + Sync {
    /// Starts sending video to `peer` in answer to its WebRTC offer. The error string is
    /// shown to the client, so keep it readable. The caller has already checked `view`.
    fn answer<'a>(&'a self, peer: &'a PeerInfo, offer: String) -> MediaFuture<'a>;
}
