//! Databricks CLI profile resolution.
//!
//! Consolidates the logic for resolving which Databricks CLI profile to
//! use from multiple sources (explicit flag, environment variable, dotenv).

use std::collections::HashMap;

/// Environment variable name for Databricks CLI profile.
const PROFILE_ENV_VAR: &str = "DATABRICKS_CONFIG_PROFILE";

/// Where a resolved profile came from.
enum ProfileSource<'a> {
    /// `--profile` CLI flag.
    Explicit(&'a str),
    /// `DATABRICKS_CONFIG_PROFILE` environment variable.
    EnvVar(&'a str),
    /// `.env` file entry.
    Dotenv(&'a str),
    /// No profile specified — SDK uses DEFAULT.
    Default,
}

/// Classify which source provides the profile, in priority order.
fn classify<'a>(
    cli_flag: Option<&'a str>,
    env_var: Option<&'a str>,
    dotenv: Option<&'a str>,
) -> ProfileSource<'a> {
    if let Some(v) = non_blank(cli_flag) {
        return ProfileSource::Explicit(v);
    }
    if let Some(v) = non_blank(env_var) {
        return ProfileSource::EnvVar(v);
    }
    if let Some(v) = non_blank(dotenv) {
        return ProfileSource::Dotenv(v);
    }
    ProfileSource::Default
}

/// Resolve a Databricks profile name: explicit flag → env var → dotenv → empty.
///
/// Pure function — all inputs are passed as arguments, no I/O.
fn resolve(cli_flag: Option<&str>, env_var: Option<&str>, dotenv: Option<&str>) -> String {
    match classify(cli_flag, env_var, dotenv) {
        ProfileSource::Explicit(p) | ProfileSource::EnvVar(p) | ProfileSource::Dotenv(p) => {
            p.to_string()
        }
        ProfileSource::Default => String::new(),
    }
}

/// Return the trimmed value if non-empty, `None` otherwise.
fn non_blank(val: Option<&str>) -> Option<&str> {
    val.map(str::trim).filter(|s| !s.is_empty())
}

/// Resolve a Databricks CLI profile from multiple sources.
///
/// Priority: explicit CLI flag → `DATABRICKS_CONFIG_PROFILE` env var → dotenv vars → empty.
#[derive(Debug)]
pub struct EnvProfile<'a> {
    dotenv_vars: &'a HashMap<String, String>,
}

impl<'a> EnvProfile<'a> {
    /// Create a resolver backed by dotenv vars.
    #[must_use]
    pub const fn new(dotenv_vars: &'a HashMap<String, String>) -> Self {
        Self { dotenv_vars }
    }

    /// Resolve the profile name.
    ///
    /// `explicit` is the value from a CLI flag (e.g. `--profile`).
    #[must_use]
    pub fn retrieve(&self, explicit: Option<&str>) -> String {
        let env_val = std::env::var(PROFILE_ENV_VAR).ok();
        let dotenv_val = self.dotenv_vars.get(PROFILE_ENV_VAR).map(String::as_str);
        resolve(explicit, env_val.as_deref(), dotenv_val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_arg_takes_priority() {
        assert_eq!(
            resolve(Some("explicit"), Some("env"), Some("dotenv")),
            "explicit"
        );
    }

    #[test]
    fn env_var_takes_priority_over_dotenv() {
        assert_eq!(resolve(None, Some("env"), Some("dotenv")), "env");
    }

    #[test]
    fn dotenv_used_when_no_env_var() {
        assert_eq!(resolve(None, None, Some("dotenv")), "dotenv");
    }

    #[test]
    fn empty_when_no_sources() {
        assert_eq!(resolve(None, None, None), "");
    }

    #[test]
    fn whitespace_explicit_arg_is_ignored() {
        assert_eq!(resolve(Some("  "), Some("env"), None), "env");
    }

    #[test]
    fn whitespace_only_falls_through_all() {
        assert_eq!(resolve(Some("  "), Some(" "), Some("  ")), "");
    }
}
