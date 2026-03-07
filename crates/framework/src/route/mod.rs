//! Route manifest and bound types.
//!
//! Manifest types are serializable (no PyO3) — they form the build artifact
//! produced by `apx build` and consumed by `apx serve`.
//!
//! Bound types carry live Python objects and are constructed at runtime during
//! route discovery.

mod bound;
mod dependency;
mod manifest;
mod primitives;

pub use dependency::*;
pub use manifest::*;
pub use primitives::*;

pub(crate) use bound::{App, BoundRoute, Handler};
