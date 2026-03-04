//! Inter-process communication between supervisor and workers.
//!
//! Uses length-prefixed msgpack over Unix Domain Sockets.

pub mod channel;
pub mod protocol;
