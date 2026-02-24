//! Dev token generation and constants.

use rand::Rng;
use rand::distributions::Alphanumeric;

/// HTTP header used to pass the dev token between services.
pub const DEV_TOKEN_HEADER: &str = "x-apx-dev-token";

/// Environment variable name for passing the dev token to child processes.
pub const DEV_TOKEN_ENV: &str = "APX_DEV_TOKEN";

/// Length of generated dev tokens (alphanumeric characters).
const TOKEN_LENGTH: usize = 32;

/// Generate a cryptographically random alphanumeric token.
pub fn generate() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(TOKEN_LENGTH)
        .map(char::from)
        .collect()
}
