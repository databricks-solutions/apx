use std::fmt;

/// Builds a structured User-Agent header matching the Databricks Python SDK format.
///
/// Format:
/// ```text
/// {product}/{product_version} apx-databricks-sdk-rust/{sdk_version} rust/{rust_version} os/{os} auth/{auth_type} [extras...] [upstream/{name}] [upstream-version/{ver}] [runtime/{ver}] [cicd/{provider}]
/// ```
#[derive(Debug)]
pub struct UserAgent {
    /// Sanitized product name.
    product: String,
    /// Sanitized product version.
    product_version: String,
    /// Optional authentication type tag.
    auth_type: Option<String>,
    /// Additional key/value pairs appended to the header.
    extras: Vec<(String, String)>,
}

impl UserAgent {
    /// Create a new `UserAgent` with the given product name and version.
    ///
    /// Both values are sanitized — only `[a-zA-Z0-9_.+-]` chars are kept.
    #[must_use]
    pub fn new(product: &str, product_version: &str) -> Self {
        Self {
            product: sanitize(product),
            product_version: sanitize(product_version),
            auth_type: None,
            extras: Vec::new(),
        }
    }

    /// Set the authentication type (e.g. "databricks-cli").
    #[must_use]
    pub fn with_auth(mut self, auth_type: &str) -> Self {
        self.auth_type = Some(sanitize(auth_type));
        self
    }
}

impl fmt::Display for UserAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 1. product/version
        write!(f, "{}/{}", self.product, self.product_version)?;

        // 2. SDK name/version
        write!(f, " apx-databricks-sdk-rust/{}", env!("CARGO_PKG_VERSION"))?;

        // 3. rust/version (captured at build time by build.rs)
        write!(f, " rust/{}", env!("RUSTC_VERSION"))?;

        // 4. os
        write!(f, " os/{}", std::env::consts::OS)?;

        // 5. auth type
        if let Some(ref auth) = self.auth_type {
            write!(f, " auth/{auth}")?;
        }

        // 6. per-instance extras
        for (key, value) in &self.extras {
            write!(f, " {key}/{value}")?;
        }

        // 7. upstream info from env vars
        if let Ok(upstream) = std::env::var("DATABRICKS_SDK_UPSTREAM") {
            let upstream = sanitize(&upstream);
            if !upstream.is_empty() {
                write!(f, " upstream/{upstream}")?;
            }
        }
        if let Ok(ver) = std::env::var("DATABRICKS_SDK_UPSTREAM_VERSION") {
            let ver = sanitize(&ver);
            if !ver.is_empty() {
                write!(f, " upstream-version/{ver}")?;
            }
        }

        // 8. runtime version
        if let Ok(ver) = std::env::var("DATABRICKS_RUNTIME_VERSION") {
            let ver = sanitize(&ver);
            if !ver.is_empty() {
                write!(f, " runtime/{ver}")?;
            }
        }

        // 9. CI/CD detection
        if let Some(provider) = detect_cicd() {
            write!(f, " cicd/{provider}")?;
        }

        Ok(())
    }
}

/// Keep only characters matching `[a-zA-Z0-9_.+-]`.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .collect()
}

/// Detect CI/CD provider from environment variables, matching the Python SDK behavior.
fn detect_cicd() -> Option<&'static str> {
    let env_is =
        |key: &str, val: &str| -> bool { std::env::var(key).ok().is_some_and(|v| v == val) };
    let env_set = |key: &str| -> bool { std::env::var(key).ok().is_some_and(|v| !v.is_empty()) };

    if env_is("GITHUB_ACTIONS", "true") {
        return Some("github");
    }
    if env_is("GITLAB_CI", "true") {
        return Some("gitlab");
    }
    if env_set("JENKINS_URL") {
        return Some("jenkins");
    }
    if env_is("TF_BUILD", "True") {
        return Some("azure-devops");
    }
    if env_is("CIRCLECI", "true") {
        return Some("circle");
    }
    if env_is("TRAVIS", "true") {
        return Some("travis");
    }
    if env_set("BITBUCKET_BUILD_NUMBER") {
        return Some("bitbucket");
    }
    if env_set("BUILDKITE") {
        return Some("buildkite");
    }
    if env_set("CODEBUILD_BUILD_ID") {
        return Some("aws-codebuild");
    }
    if env_set("TEAMCITY_VERSION") {
        return Some("teamcity");
    }
    None
}

#[cfg(test)]
impl UserAgent {
    /// Add an extra key/value pair to the User-Agent string.
    #[must_use]
    pub fn with_extra(mut self, key: &str, value: &str) -> Self {
        self.extras.push((sanitize(key), sanitize(value)));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_user_agent() {
        let ua = UserAgent::new("apx", "0.3.0-rc1").with_auth("databricks-cli");
        let s = ua.to_string();

        assert!(s.starts_with("apx/0.3.0-rc1"));
        assert!(s.contains("apx-databricks-sdk-rust/"));
        assert!(s.contains("rust/"));
        assert!(s.contains(&format!("os/{}", std::env::consts::OS)));
        assert!(s.contains("auth/databricks-cli"));
    }

    #[test]
    fn test_extra_fields() {
        let ua = UserAgent::new("test", "1.0.0")
            .with_auth("pat")
            .with_extra("custom", "value");
        let s = ua.to_string();

        assert!(s.contains("custom/value"));
    }

    #[test]
    fn test_sanitize() {
        assert_eq!(sanitize("hello world!@#"), "helloworld");
        assert_eq!(sanitize("v1.2.3-rc1+build"), "v1.2.3-rc1+build");
        assert_eq!(sanitize("some_name"), "some_name");
    }

    #[test]
    fn test_no_auth() {
        let ua = UserAgent::new("myapp", "2.0.0");
        let s = ua.to_string();

        assert!(s.starts_with("myapp/2.0.0"));
        assert!(!s.contains("auth/"));
    }

    #[test]
    fn test_detect_cicd_returns_none_in_local_env() {
        // In a normal dev/test environment (no CI env vars set),
        // detect_cicd should return None.
        // This won't hold in CI, so we just verify the function doesn't panic.
        let _ = detect_cicd();
    }
}
