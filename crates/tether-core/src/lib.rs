//! Identity, trust, pairing and the encrypted channel. No platform code lives here.

pub mod channel;
pub mod clipboard;
pub mod discovery;
pub mod identity;
pub mod input;
pub mod link;
pub mod media;
pub mod pairing;
pub mod paths;
pub mod permissions;
pub mod prompt;
pub mod sas;
pub mod session;
pub mod trust;
pub mod wake;

pub use tether_proto as proto;
