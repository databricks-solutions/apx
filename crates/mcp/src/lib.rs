#![forbid(unsafe_code)]
#![deny(warnings, unused_must_use, dead_code, missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

pub mod core;
pub mod server;
