//! Platform-independent logic of 2fcast: everything that can be unit-tested
//! without a display, a camera or GStreamer.

pub mod annotations;
pub mod camera;
pub mod config;
pub mod deps;
pub mod layout;
pub mod net;
pub mod pipelines;
pub mod protocol;
pub mod pulse;
pub mod session;

/// Stable identifier of one installation (stored in the config file).
pub type PeerId = String;
