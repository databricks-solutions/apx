use std::time::Instant;

use apx_core::common::{format_elapsed_ms, spinner};

use crate::run_cli_async_helper;

/// GitHub repository for release lookups.
const GITHUB_REPO: &str = "databricks-solutions/apx";

pub async fn run() -> i32 {
    run_cli_async_helper(run_inner).await
}

async fn run_inner() -> Result<(), String> {
    let current_version = env!("CARGO_PKG_VERSION");
    println!("  Current version: {current_version}");

    // 1. Fetch latest release from GitHub
    let sp = spinner("Checking for updates...");
    let start = Instant::now();
    let latest_tag = fetch_latest_tag().await?;
    let latest_version = latest_tag.strip_prefix('v').unwrap_or(&latest_tag);
    sp.finish_and_clear();
    println!(
        "  Latest version:  {latest_version} ({})",
        format_elapsed_ms(start)
    );

    // 2. Compare versions
    if version_gte(current_version, latest_version) {
        println!("  Already up to date.");
        return Ok(());
    }

    // 3. Detect platform asset name
    let asset_name = platform_asset_name()?;

    // 4. Download new binary
    let sp = spinner(&format!("Downloading apx v{latest_version}..."));
    let start = Instant::now();
    let download_url =
        format!("https://github.com/{GITHUB_REPO}/releases/download/{latest_tag}/{asset_name}");
    let binary_bytes = download_binary(&download_url).await?;
    sp.finish_and_clear();
    println!(
        "  Downloaded apx v{latest_version} ({}) ({})",
        humanize_bytes(binary_bytes.len()),
        format_elapsed_ms(start)
    );

    // 5. Replace current binary atomically
    let sp = spinner("Installing...");
    let start = Instant::now();
    replace_current_binary(&binary_bytes)?;
    sp.finish_and_clear();
    println!("  Installed ({})", format_elapsed_ms(start));

    println!("  Upgraded apx: {current_version} → {latest_version}");
    Ok(())
}

/// Fetch the latest release tag from the GitHub API.
async fn fetch_latest_tag() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "apx-cli")
        .send()
        .await
        .map_err(|e| format!("Failed to check for updates: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "GitHub API returned status {} when checking for updates",
            resp.status()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;

    body.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "GitHub API response missing tag_name".to_string())
}

/// Download a binary from the given URL.
async fn download_binary(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("User-Agent", "apx-cli")
        .send()
        .await
        .map_err(|e| format!("Failed to download: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Download failed with status {}. Check that a binary exists for your platform.",
            resp.status()
        ));
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read download: {e}"))
}

/// Replace the currently running binary with new contents.
fn replace_current_binary(new_binary: &[u8]) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to determine current executable path: {e}"))?;

    // Resolve symlinks to get the real path
    let real_path = std::fs::canonicalize(&current_exe)
        .map_err(|e| format!("Failed to resolve executable path: {e}"))?;

    let parent = real_path
        .parent()
        .ok_or_else(|| "Cannot determine parent directory of current executable".to_string())?;

    // Write to a temp file in the same directory (ensures same filesystem for atomic rename)
    let tmp_path = parent.join(".apx-upgrade-tmp");

    std::fs::write(&tmp_path, new_binary)
        .map_err(|e| format!("Failed to write new binary: {e}"))?;

    // Copy permissions from the old binary
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&real_path)
            .map_err(|e| format!("Failed to read current binary permissions: {e}"))?;
        std::fs::set_permissions(&tmp_path, metadata.permissions())
            .map_err(|e| format!("Failed to set permissions on new binary: {e}"))?;

        // Ensure executable bit is set (at minimum owner execute)
        let mut perms = std::fs::metadata(&tmp_path)
            .map_err(|e| format!("Failed to read new binary metadata: {e}"))?
            .permissions();
        let mode = perms.mode() | 0o111;
        perms.set_mode(mode);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| format!("Failed to set executable permission: {e}"))?;
    }

    // Atomic rename
    std::fs::rename(&tmp_path, &real_path).map_err(|e| {
        // Clean up temp file on failure
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to replace binary: {e}")
    })?;

    Ok(())
}

/// Determine the asset name for the current platform.
fn platform_asset_name() -> Result<String, String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(format!("Unsupported architecture: {other}")),
    };
    let (platform, ext) = match std::env::consts::OS {
        "linux" => ("linux", ""),
        "macos" => ("darwin", ""),
        "windows" => ("windows", ".exe"),
        other => return Err(format!("Unsupported platform: {other}")),
    };
    Ok(format!("apx-{arch}-{platform}{ext}"))
}

/// Compare two version strings. Returns true if `current >= latest`.
///
/// Supports `X.Y.Z` and `X.Y.Z-rcN` patterns.
/// A release version (no pre-release) is greater than the same version with a pre-release suffix.
fn version_gte(current: &str, latest: &str) -> bool {
    let (cur_base, cur_pre) = parse_version(current);
    let (lat_base, lat_pre) = parse_version(latest);

    match compare_base(&cur_base, &lat_base) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            // Same base: compare pre-release
            // No pre-release > any pre-release (stable beats RC)
            match (cur_pre, lat_pre) {
                (None, None | Some(_)) => true, // equal or stable > rc
                (Some(_), None) => false,       // rc < stable
                (Some(a), Some(b)) => a >= b,
            }
        }
    }
}

/// Parse "X.Y.Z" or "X.Y.Z-rcN" into base parts and optional pre-release number.
fn parse_version(v: &str) -> (Vec<u64>, Option<u64>) {
    let (base_str, pre) = if let Some(idx) = v.find("-rc") {
        let pre_num = v[idx + 3..].parse::<u64>().unwrap_or(0);
        (&v[..idx], Some(pre_num))
    } else if let Some(idx) = v.find('-') {
        // Other pre-release suffixes: treat as rc0
        (&v[..idx], Some(0))
    } else {
        (v, None)
    };

    let base: Vec<u64> = base_str.split('.').filter_map(|s| s.parse().ok()).collect();

    (base, pre)
}

/// Compare two base version part vectors.
fn compare_base(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let va = a.get(i).copied().unwrap_or(0);
        let vb = b.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn humanize_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{kb:.1} KB");
    }
    let mb = kb / 1024.0;
    format!("{mb:.1} MB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_gte() {
        // Same version
        assert!(version_gte("0.3.0", "0.3.0"));
        // Current newer
        assert!(version_gte("0.4.0", "0.3.0"));
        assert!(version_gte("1.0.0", "0.9.9"));
        // Current older
        assert!(!version_gte("0.2.0", "0.3.0"));
        assert!(!version_gte("0.3.0", "0.4.0"));
        // RC vs stable
        assert!(!version_gte("0.3.0-rc1", "0.3.0"));
        assert!(version_gte("0.3.0", "0.3.0-rc1"));
        // RC vs RC
        assert!(version_gte("0.3.0-rc2", "0.3.0-rc1"));
        assert!(!version_gte("0.3.0-rc1", "0.3.0-rc2"));
        assert!(version_gte("0.3.0-rc1", "0.3.0-rc1"));
        // RC of newer version
        assert!(version_gte("0.4.0-rc1", "0.3.0"));
        assert!(!version_gte("0.3.0-rc1", "0.4.0"));
    }

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("0.3.0"), (vec![0, 3, 0], None));
        assert_eq!(parse_version("0.3.0-rc1"), (vec![0, 3, 0], Some(1)));
        assert_eq!(parse_version("1.2.3-rc12"), (vec![1, 2, 3], Some(12)));
        assert_eq!(parse_version("0.3.0-beta"), (vec![0, 3, 0], Some(0)));
    }

    #[test]
    fn test_platform_asset_name() {
        // Just verify it doesn't error on the current platform
        let name = platform_asset_name();
        assert!(name.is_ok());
        let name = name.ok();
        assert!(name.is_some());
        let name_str = name.unwrap_or_default();
        assert!(name_str.starts_with("apx-"));
    }

    #[test]
    fn test_humanize_bytes() {
        assert_eq!(humanize_bytes(500), "500 B");
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(humanize_bytes(15 * 1024 * 1024), "15.0 MB");
    }
}
