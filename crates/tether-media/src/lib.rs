//! Host side video: frame sources, the H.264 encoder and the WebRTC session.
//!
//! The control session (tether-core) only knows the `MediaProvider` trait. [`VideoHost`] is
//! the implementation: given a client's WebRTC offer it answers and starts streaming frames
//! from a [`FrameSource`].

pub mod audio;
pub mod capture;
pub mod encoder;
pub mod host;
pub mod source;

pub use audio::{AudioSource, AudioSpec, LoopbackSource, OpusEncoder, ToneSource};
pub use capture::{list_displays, DisplayInfo, ScreenSource};
pub use encoder::H264Encoder;
pub use host::{MediaOptions, VideoHost, VideoSpec};
pub use source::{read_counter, Frame, FrameSource, TestPattern};
