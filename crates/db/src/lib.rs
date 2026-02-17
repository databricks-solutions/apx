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

//! Database layer for APX using SQLx with SQLite.
//!
//! Provides async connection pools for two databases:
//! - **Logs DB** (`~/.apx/logs/db`) — OTLP log storage
//! - **Dev DB** (`~/.apx/dev/db`) — search indexes and future dev-related tables

pub mod dev;
pub mod logs;

pub use dev::DevDb;
pub use logs::LogsDb;
pub use sqlx::sqlite::SqlitePool;

use std::path::PathBuf;

/// Get the logs database path (`~/.apx/logs/db`).
pub fn logs_db_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx").join("logs").join("db"))
}

/// Get the dev database path (`~/.apx/dev/db`).
pub fn dev_db_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx").join("dev").join("db"))
}
