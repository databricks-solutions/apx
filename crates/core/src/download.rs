//! Runtime resolution and auto-download of bun and uv binaries.
//!
//! Resolution order (same for both tools):
//! 1. Environment variable override (`APX_BUN_PATH` / `APX_UV_PATH`)
//! 2. System PATH via `which::which()`
//! 3. `~/.apx/bin/{bun,uv}` — only if version marker matches pinned version
//! 4. Download from GitHub releases → `~/.apx/bin/`, write version marker

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use tracing::debug;

const BUN_VERSION: &str = "1.3.8";
const UV_VERSION: &str = "0.10.3";

#[cfg(target_os = "windows")]
const BUN_EXE: &str = "bun.exe";
#[cfg(not(target_os = "windows"))]
const BUN_EXE: &str = "bun";

#[cfg(target_os = "windows")]
const UV_EXE: &str = "uv.exe";
#[cfg(not(target_os = "windows"))]
const UV_EXE: &str = "uv";

/// Where a binary was found.
#[derive(Debug, Clone)]
pub enum BinarySource {
    EnvOverride,
    SystemPath,
    ApxManaged,
}

impl BinarySource {
    pub fn source_label(&self) -> &'static str {
        match self {
            BinarySource::EnvOverride => "env-override",
            BinarySource::SystemPath => "system",
            BinarySource::ApxManaged => "apx-provided",
        }
    }
}

/// A resolved binary path with its source.
#[derive(Debug, Clone)]
pub struct ResolvedBinary {
    pub path: PathBuf,
    pub source: BinarySource,
}

impl ResolvedBinary {
    pub fn source_label(&self) -> &'static str {
        match self.source {
            BinarySource::EnvOverride => "env-override",
            BinarySource::SystemPath => "system",
            BinarySource::ApxManaged => "apx-provided",
        }
    }
}

// ---------------------------------------------------------------------------
// Caches — resolved at most once per process
// ---------------------------------------------------------------------------

static BUN_CELL: OnceCell<ResolvedBinary> = OnceCell::const_new();
static UV_CELL: OnceCell<ResolvedBinary> = OnceCell::const_new();

// ---------------------------------------------------------------------------
// Public async API (resolves + downloads if needed)
// ---------------------------------------------------------------------------

/// Resolve bun binary. Downloads if not found on PATH or in `~/.apx/bin/`.
pub async fn resolve_bun() -> Result<ResolvedBinary, String> {
    BUN_CELL.get_or_try_init(resolve_bun_inner).await.cloned()
}

/// Resolve uv binary. Downloads if not found on PATH or in `~/.apx/bin/`.
pub async fn resolve_uv() -> Result<ResolvedBinary, String> {
    UV_CELL.get_or_try_init(resolve_uv_inner).await.cloned()
}

// ---------------------------------------------------------------------------
// Public sync API (no download — env / PATH / cached only)
// ---------------------------------------------------------------------------

/// Sync resolve for bun. Checks OnceCell cache, then env/PATH/filesystem.
/// Does NOT download. Used by sync callers.
pub fn try_resolve_bun() -> Result<ResolvedBinary, String> {
    if let Some(cached) = BUN_CELL.get() {
        return Ok(cached.clone());
    }
    resolve_local(BUN_EXE, "APX_BUN_PATH", BUN_VERSION, ".bun-version")
}

/// Sync resolve for uv. Checks OnceCell cache, then env/PATH/filesystem.
/// Does NOT download. Used by sync callers.
pub fn try_resolve_uv() -> Result<ResolvedBinary, String> {
    if let Some(cached) = UV_CELL.get() {
        return Ok(cached.clone());
    }
    resolve_local(UV_EXE, "APX_UV_PATH", UV_VERSION, ".uv-version")
}

// ---------------------------------------------------------------------------
// Internal: async resolution (with download fallback)
// ---------------------------------------------------------------------------

async fn resolve_bun_inner() -> Result<ResolvedBinary, String> {
    // 1-3: try local resolution
    if let Ok(resolved) = resolve_local(BUN_EXE, "APX_BUN_PATH", BUN_VERSION, ".bun-version") {
        return Ok(resolved);
    }

    // 4: download
    eprintln!("bun not found on PATH — downloading v{BUN_VERSION}...");
    let path = download_bun().await.map_err(|e| {
        format!("Failed to auto-install bun v{BUN_VERSION}: {e}\n  Install bun manually (https://bun.sh) or set APX_BUN_PATH.")
    })?;
    eprintln!("bun v{BUN_VERSION} installed to {}", path.display());
    Ok(ResolvedBinary {
        path,
        source: BinarySource::ApxManaged,
    })
}

async fn resolve_uv_inner() -> Result<ResolvedBinary, String> {
    if let Ok(resolved) = resolve_local(UV_EXE, "APX_UV_PATH", UV_VERSION, ".uv-version") {
        return Ok(resolved);
    }

    eprintln!("uv not found on PATH — downloading v{UV_VERSION}...");
    let path = download_uv().await.map_err(|e| {
        format!("Failed to auto-install uv v{UV_VERSION}: {e}\n  Install uv manually (https://docs.astral.sh/uv/) or set APX_UV_PATH.")
    })?;
    eprintln!("uv v{UV_VERSION} installed to {}", path.display());
    Ok(ResolvedBinary {
        path,
        source: BinarySource::ApxManaged,
    })
}

// ---------------------------------------------------------------------------
// Internal: local resolution (env → PATH → ~/.apx/bin/)
// ---------------------------------------------------------------------------

fn resolve_local(
    exe_name: &str,
    env_var: &str,
    pinned_version: &str,
    version_file: &str,
) -> Result<ResolvedBinary, String> {
    // 1. Env var override
    if let Ok(path) = std::env::var(env_var) {
        let p = PathBuf::from(&path);
        if p.is_file() {
            debug!("{env_var}={} — using env override", p.display());
            return Ok(ResolvedBinary {
                path: p,
                source: BinarySource::EnvOverride,
            });
        }
        return Err(format!("{env_var}={path} does not exist"));
    }

    // 2. System PATH
    if let Ok(path) = which::which(exe_name) {
        debug!("{exe_name} found on PATH at {}", path.display());
        return Ok(ResolvedBinary {
            path,
            source: BinarySource::SystemPath,
        });
    }

    // 3. ~/.apx/bin/ with version marker
    if let Some(bin_dir) = apx_bin_dir() {
        let candidate = bin_dir.join(exe_name);
        let marker = bin_dir.join(version_file);
        if candidate.is_file()
            && let Ok(contents) = std::fs::read_to_string(&marker)
        {
            if contents.trim() == pinned_version {
                debug!(
                    "{exe_name} found in ~/.apx/bin/ (v{pinned_version}): {}",
                    candidate.display()
                );
                return Ok(ResolvedBinary {
                    path: candidate,
                    source: BinarySource::ApxManaged,
                });
            }
            debug!(
                "{exe_name} in ~/.apx/bin/ has version '{}', need '{pinned_version}' — will re-download",
                contents.trim()
            );
        }
    }

    Err(format!(
        "Could not find {exe_name}. Install it or set {env_var}."
    ))
}

// ---------------------------------------------------------------------------
// Download: bun
// ---------------------------------------------------------------------------

fn bun_platform_tag() -> Result<&'static str, String> {
    let tag = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-aarch64",
        ("windows", "x86_64") => "windows-x64",
        (os, arch) => return Err(format!("Unsupported platform for bun: {os}-{arch}")),
    };
    Ok(tag)
}

async fn download_bun() -> Result<PathBuf, String> {
    let platform = bun_platform_tag()?;
    let url = format!(
        "https://github.com/oven-sh/bun/releases/download/bun-v{BUN_VERSION}/bun-{platform}.zip"
    );

    let bin_dir = ensure_apx_bin_dir()?;
    let dest = bin_dir.join(BUN_EXE);

    debug!("downloading bun v{BUN_VERSION} from {url}");
    let bytes = http_get(&url).await?;

    // Verify SHA-256 checksum
    let archive_name = format!("bun-{platform}.zip");
    let checksums_url = format!(
        "https://github.com/oven-sh/bun/releases/download/bun-v{BUN_VERSION}/SHASUMS256.txt"
    );
    let checksums = String::from_utf8(http_get(&checksums_url).await?)
        .map_err(|e| format!("Invalid UTF-8 in bun checksums: {e}"))?;
    let expected = parse_sha256_for_file(&checksums, &archive_name)?;
    verify_sha256(&bytes, &expected, "bun archive")?;

    // Extract bun from the zip (archives have a subdirectory)
    let cursor = std::io::Cursor::new(&bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to open bun zip: {e}"))?;

    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {e}"))?;
        let name = entry.name().to_string();
        let file_name = Path::new(&name)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        if file_name == BUN_EXE {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read bun from zip: {e}"))?;
            std::fs::write(&dest, &buf).map_err(|e| format!("Failed to write bun binary: {e}"))?;
            found = true;
            break;
        }
    }

    if !found {
        return Err("bun executable not found inside zip archive".to_string());
    }

    set_executable(&dest)?;
    write_version_marker(&bin_dir, ".bun-version", BUN_VERSION)?;
    debug!("bun v{BUN_VERSION} extracted to {}", dest.display());
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Download: uv
// ---------------------------------------------------------------------------

fn uv_target_triple() -> Result<&'static str, String> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => return Err(format!("Unsupported platform for uv: {os}-{arch}")),
    };
    Ok(triple)
}

async fn download_uv() -> Result<PathBuf, String> {
    let target = uv_target_triple()?;
    let bin_dir = ensure_apx_bin_dir()?;
    let dest = bin_dir.join(UV_EXE);

    #[cfg(target_os = "windows")]
    let (url, is_zip) = {
        let url = format!(
            "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-{target}.zip"
        );
        (url, true)
    };

    #[cfg(not(target_os = "windows"))]
    let (url, is_zip) = {
        let url = format!(
            "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-{target}.tar.gz"
        );
        (url, false)
    };

    debug!("downloading uv v{UV_VERSION} from {url}");
    let bytes = http_get(&url).await?;

    // Verify SHA-256 checksum
    let checksums_url = format!("{url}.sha256");
    let archive_name = url
        .rsplit('/')
        .next()
        .ok_or("Failed to extract archive filename from URL")?;
    let checksums = String::from_utf8(http_get(&checksums_url).await?)
        .map_err(|e| format!("Invalid UTF-8 in uv checksums: {e}"))?;
    let expected = parse_sha256_for_file(&checksums, archive_name)?;
    verify_sha256(&bytes, &expected, "uv archive")?;

    if is_zip {
        extract_uv_from_zip(&bytes, &dest)?;
    } else {
        extract_uv_from_tar_gz(&bytes, &dest)?;
    }

    set_executable(&dest)?;
    write_version_marker(&bin_dir, ".uv-version", UV_VERSION)?;
    debug!("uv v{UV_VERSION} extracted to {}", dest.display());
    Ok(dest)
}

fn extract_uv_from_zip(data: &[u8], dest: &Path) -> Result<(), String> {
    let cursor = std::io::Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to open uv zip: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {e}"))?;
        let name = entry.name().to_string();
        let file_name = Path::new(&name)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        if file_name == UV_EXE {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read uv from zip: {e}"))?;
            std::fs::write(dest, &buf).map_err(|e| format!("Failed to write uv binary: {e}"))?;
            return Ok(());
        }
    }

    Err("uv executable not found inside zip archive".to_string())
}

fn extract_uv_from_tar_gz(data: &[u8], dest: &Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);

    for entry in archive
        .entries()
        .map_err(|e| format!("Failed to read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("Failed to read tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("Failed to read tar entry path: {e}"))?;
        let file_name = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        if file_name == UV_EXE {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed to read uv from tar: {e}"))?;
            std::fs::write(dest, &buf).map_err(|e| format!("Failed to write uv binary: {e}"))?;
            return Ok(());
        }
    }

    Err("uv executable not found inside tar.gz archive".to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn verify_sha256(data: &[u8], expected_hex: &str, label: &str) -> Result<(), String> {
    let actual = hex::encode(Sha256::digest(data));
    if actual != expected_hex {
        return Err(format!(
            "{label}: SHA-256 mismatch — expected {expected_hex}, got {actual}"
        ));
    }
    debug!("{label}: SHA-256 verified");
    Ok(())
}

fn parse_sha256_for_file(checksums_text: &str, target_filename: &str) -> Result<String, String> {
    for line in checksums_text.lines() {
        // Format: "<64-char hex>  <filename>"
        let Some((hash, filename)) = line.split_once("  ") else {
            continue;
        };
        if filename.trim() == target_filename {
            return Ok(hash.to_string());
        }
    }
    Err(format!(
        "SHA-256 checksum not found for '{target_filename}' in checksums file"
    ))
}

fn apx_bin_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".apx").join("bin"))
}

fn ensure_apx_bin_dir() -> Result<PathBuf, String> {
    let dir = apx_bin_dir().ok_or("Could not determine home directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create ~/.apx/bin/: {e}"))?;
    Ok(dir)
}

#[allow(unused_variables)]
fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| format!("Failed to read metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }
    Ok(())
}

fn write_version_marker(bin_dir: &Path, filename: &str, version: &str) -> Result<(), String> {
    let marker = bin_dir.join(filename);
    std::fs::write(&marker, version)
        .map_err(|e| format!("Failed to write version marker {filename}: {e}"))
}

async fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .user_agent("apx-cli")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            format!("Download timed out (120s) for {url}")
        } else if e.is_connect() {
            format!("Could not connect to {url} — check your internet connection")
        } else {
            format!("HTTP request failed for {url}: {e}")
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status} from {url}"));
    }

    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read response body from {url}: {e}"))
}
