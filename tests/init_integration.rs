//! Integration test for `apx init` command.
//!
//! This test builds a wheel using maturin and runs `uvx apx init` to verify
//! the full initialization workflow works correctly.
//!
//! Run with: `cargo test --test init_integration -- --ignored`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Find the first .whl file in a directory
fn find_wheel(dir: &Path) -> PathBuf {
    for entry in fs::read_dir(dir).expect("Failed to read wheel directory") {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();
        if path.extension() == Some(OsStr::new("whl")) {
            return path;
        }
    }
    panic!("No wheel found in {}", dir.display());
}

/// Get the project root directory (where Cargo.toml is)
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
#[ignore] // Slow test - run with `cargo test --test init_integration -- --ignored`
fn test_init_creates_project_structure() {
    // 1. Create temp directories for wheel output and project
    let wheel_dir = TempDir::new().expect("Failed to create wheel temp dir");
    let project_dir = TempDir::new().expect("Failed to create project temp dir");
    let app_path = project_dir.path().join("test-app");

    // 2. Build wheel using maturin
    println!("Building wheel with maturin...");
    let build_output = Command::new("maturin")
        .args(["build", "--out", wheel_dir.path().to_str().unwrap()])
        .current_dir(project_root())
        .output()
        .expect("Failed to run maturin build");

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let stdout = String::from_utf8_lossy(&build_output.stdout);
        panic!("maturin build failed:\nstdout: {stdout}\nstderr: {stderr}");
    }
    println!("Wheel built successfully");

    // 3. Find the wheel file
    let wheel_path = find_wheel(wheel_dir.path());
    println!("Found wheel: {}", wheel_path.display());

    // 4. Run uvx apx init with all options to make it non-interactive
    println!("Running apx init...");
    let init_output = Command::new("uvx")
        .args([
            "--from",
            wheel_path.to_str().unwrap(),
            "apx",
            "init",
            "--name",
            "test-app",
            "--template",
            "essential",
            "--layout",
            "sidebar",
            "--assistant",
            "cursor",
            "--profile",
            "WORKSPACE",
            app_path.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run uvx apx init");

    if !init_output.status.success() {
        let stderr = String::from_utf8_lossy(&init_output.stderr);
        let stdout = String::from_utf8_lossy(&init_output.stdout);
        panic!("apx init failed:\nstdout: {stdout}\nstderr: {stderr}");
    }
    println!("apx init completed successfully");
    println!("Output: {}", String::from_utf8_lossy(&init_output.stdout));

    // 5. Verify project structure

    // Check pyproject.toml exists and has correct content
    let pyproject_path = app_path.join("pyproject.toml");
    assert!(
        pyproject_path.exists(),
        "pyproject.toml should exist at {}",
        pyproject_path.display()
    );

    let pyproject_content =
        fs::read_to_string(&pyproject_path).expect("Failed to read pyproject.toml");

    // Verify project name
    assert!(
        pyproject_content.contains("name = \"test-app\""),
        "pyproject.toml should contain project name"
    );

    // Verify apx metadata section
    assert!(
        pyproject_content.contains("[tool.apx.metadata]"),
        "pyproject.toml should contain [tool.apx.metadata]"
    );
    assert!(
        pyproject_content.contains("app-name = \"test-app\""),
        "pyproject.toml should contain app-name"
    );

    // Verify uv index configuration
    assert!(
        pyproject_content.contains("[[tool.uv.index]]"),
        "pyproject.toml should contain [[tool.uv.index]]"
    );
    assert!(
        pyproject_content.contains("name = \"apx-index\""),
        "pyproject.toml should contain apx-index"
    );

    // Verify uv sources
    assert!(
        pyproject_content.contains("[tool.uv.sources]"),
        "pyproject.toml should contain [tool.uv.sources]"
    );

    // Verify apx dev dependency
    assert!(
        pyproject_content.contains("apx=="),
        "pyproject.toml should contain apx version in dev dependencies"
    );

    // Check package.json exists
    let package_json_path = app_path.join("package.json");
    assert!(
        package_json_path.exists(),
        "package.json should exist at {}",
        package_json_path.display()
    );

    let package_json_content =
        fs::read_to_string(&package_json_path).expect("Failed to read package.json");
    assert!(
        package_json_content.contains("\"name\""),
        "package.json should contain name field"
    );

    // Check Python package directory (test-app -> test_app with underscore)
    let python_pkg_dir = app_path.join("src").join("test_app");
    assert!(
        python_pkg_dir.exists(),
        "Python package directory should exist at {}",
        python_pkg_dir.display()
    );

    // Check __init__.py exists in Python package
    let init_py = python_pkg_dir.join("__init__.py");
    assert!(
        init_py.exists(),
        "__init__.py should exist at {}",
        init_py.display()
    );

    // Check backend directory exists
    let backend_dir = python_pkg_dir.join("backend");
    assert!(
        backend_dir.exists(),
        "backend directory should exist at {}",
        backend_dir.display()
    );

    // Check UI directory exists (inside the Python package)
    let ui_dir = python_pkg_dir.join("ui");
    assert!(
        ui_dir.exists(),
        "UI directory should exist at {}",
        ui_dir.display()
    );

    // Check cursor assistant config (since we chose cursor)
    let cursor_dir = app_path.join(".cursor");
    assert!(
        cursor_dir.exists(),
        ".cursor directory should exist at {}",
        cursor_dir.display()
    );
    let cursor_mcp = cursor_dir.join("mcp.json");
    assert!(
        cursor_mcp.exists(),
        ".cursor/mcp.json should exist at {}",
        cursor_mcp.display()
    );

    // Check git was initialized
    let git_dir = app_path.join(".git");
    assert!(
        git_dir.exists(),
        ".git directory should exist at {}",
        git_dir.display()
    );

    // Check .env file was created with profile
    let env_file = app_path.join(".env");
    assert!(env_file.exists(), ".env should exist");
    let env_content = fs::read_to_string(&env_file).expect("Failed to read .env");
    assert!(
        env_content.contains("DATABRICKS_CONFIG_PROFILE"),
        ".env should contain DATABRICKS_CONFIG_PROFILE"
    );

    println!("All assertions passed!");
}
