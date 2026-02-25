//! Build script for apx-databricks-sdk.
fn main() {
    let version = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.strip_prefix("rustc ")
                .and_then(|v| v.split_whitespace().next())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=RUSTC_VERSION={version}");
}
