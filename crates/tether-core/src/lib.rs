//! Identity, trust, pairing and the encrypted channel. No platform code lives here.

pub mod channel;
pub mod identity;
pub mod pairing;
pub mod paths;
pub mod session;
pub mod trust;

pub use tether_proto as proto;
