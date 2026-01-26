"""Tests for resilience - project should work after removing untracked files.

These tests verify that:
1. `dev start` works after removing git-untracked files (regenerates _metadata.py, installs deps)
2. `build` works after removing git-untracked files (produces correct wheel with _metadata.py and __dist__)
"""

import asyncio
import json
import zipfile
from pathlib import Path

import httpx
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type,
)

from conftest import run_cli_async, init_project_async


async def git_clean_ignored_files(project_path: Path) -> str:
    """Remove all untracked AND ignored files and directories from the project.

    Uses `git clean -fdx` which removes:
    - Untracked files (-f)
    - Untracked directories (-d)
    - Ignored files (-x) - files matching .gitignore patterns

    Returns:
        The output showing what was removed.
    """
    process = await asyncio.create_subprocess_exec(
        "git",
        "clean",
        "-fdx",
        cwd=str(project_path),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, _ = await process.communicate()
    assert process.returncode == 0, "git clean failed"
    return stdout.decode("utf-8")


async def init_and_build_project(project_path: Path) -> None:
    """Initialize project with build (no skip-build flag)."""
    result = await init_project_async(
        project_path,
        skip_build=False,
        skip_dependencies=False,
    )
    assert result.returncode == 0, f"Init failed: {result.stderr}"


async def init_project_without_build(project_path: Path) -> None:
    """Initialize project without building."""
    result = await init_project_async(
        project_path,
        skip_build=True,
        skip_dependencies=False,
    )
    assert result.returncode == 0, f"Init failed: {result.stderr}"


def assert_metadata_file_exists(project_path: Path) -> None:
    """Assert that _metadata.py exists in the project."""
    metadata_path = project_path / "src" / "test_app" / "_metadata.py"
    assert metadata_path.exists(), f"_metadata.py not found at {metadata_path}"
    content = metadata_path.read_text()
    assert "app_name" in content, "_metadata.py should contain app_name"
    assert "api_prefix" in content, "_metadata.py should contain api_prefix"


def assert_dist_dir_exists(project_path: Path) -> None:
    """Assert that __dist__ directory exists in the project."""
    dist_dir = project_path / "src" / "test_app" / "__dist__"
    assert dist_dir.exists(), f"__dist__ directory not found at {dist_dir}"


async def test_dev_start_after_clean(tmp_path: Path) -> None:
    """Test that dev start works after removing untracked files.

    Scenario:
    1. Initialize project with build (this also commits to git)
    2. Remove all ignored files with git clean -fdx (simulates fresh clone)
    3. Run dev start - should regenerate necessary files and install deps
    4. Server should become healthy and respond to requests

    Note: The init command already creates a git repo and commits. Gitignored files
    like _metadata.py and node_modules are created AFTER the commit, so they are
    untracked. We use -x flag to also remove ignored files.
    """
    project_path = tmp_path / "clean_start_project"
    project_path.mkdir(parents=True, exist_ok=True)

    # Step 1: Initialize and build the project (this also does git init + commit)
    print("Step 1: Initializing and building project...")
    await init_and_build_project(project_path)

    # Verify initial state - files should exist after init
    assert_metadata_file_exists(project_path)
    assert_dist_dir_exists(project_path)

    # Step 2: Remove ignored files (simulates fresh git clone)
    print("Step 2: Removing ignored files with git clean -fdx...")
    clean_output = await git_clean_ignored_files(project_path)
    print(f"Cleaned files:\n{clean_output}")

    # Verify files were removed (they are gitignored)
    metadata_path = project_path / "src" / "test_app" / "_metadata.py"
    node_modules = project_path / "node_modules"

    # These should be removed because they're in .gitignore and -x flag is used
    assert not metadata_path.exists(), (
        "_metadata.py should be removed by git clean -fdx"
    )
    assert not node_modules.exists(), "node_modules should be removed by git clean -fdx"

    try:
        # Step 3: Start dev server - should regenerate files and install deps
        print("Step 3: Starting dev server...")
        start_result = await run_cli_async(
            ["dev", "start"],
            cwd=project_path,
            timeout=180.0,  # Allow time for dependency installation
        )
        assert start_result.returncode == 0, f"dev start failed: {start_result.stderr}"

        # Step 4: Verify server is running and accessible
        print("Step 4: Verifying server...")

        # Read port from dev.lock
        dev_lock_path = project_path / ".apx" / "dev.lock"
        assert dev_lock_path.exists(), "dev.lock should exist after start"
        dev_lock = json.loads(dev_lock_path.read_text())
        port = dev_lock["port"]

        # Wait for server to be ready and test connectivity
        await asyncio.sleep(2)  # Give processes time to start

        http_client = httpx.AsyncClient()

        @retry(
            stop=stop_after_attempt(10),
            wait=wait_exponential(multiplier=1, min=1, max=5),
            retry=retry_if_exception_type((httpx.RequestError, httpx.HTTPStatusError)),
        )
        async def check_backend():
            resp = await http_client.get(f"http://localhost:{port}/api/version")
            resp.raise_for_status()
            return resp

        backend_response = await check_backend()
        assert backend_response.status_code == 200, "Backend should respond"
        assert backend_response.json().get("version") is not None, (
            "Should return version"
        )

        # Step 5: Verify generated files exist again
        print("Step 5: Verifying regenerated files...")
        assert_metadata_file_exists(project_path)

        print("Test passed: dev start works after git clean")

    finally:
        # Cleanup: Stop dev server
        print("Cleanup: Stopping dev server...")
        stop_result = await run_cli_async(["dev", "stop"], cwd=project_path)
        print(f"Stop result: returncode={stop_result.returncode}")


async def test_build_after_clean(tmp_path: Path) -> None:
    """Test that build works after removing untracked files.

    Scenario:
    1. Initialize project without build (this also commits to git)
    2. Remove all ignored files with git clean -fdx (simulates fresh clone)
    3. Run build - should install deps if missing and generate files
    4. Check the wheel for _metadata.py and __dist__ contents
    """
    project_path = tmp_path / "clean_build_project"
    project_path.mkdir(parents=True, exist_ok=True)

    # Step 1: Initialize project (with dependencies but without build)
    print("Step 1: Initializing project...")
    await init_project_without_build(project_path)

    # Step 2: Remove ignored files (simulates fresh git clone)
    print("Step 2: Removing ignored files with git clean -fdx...")
    clean_output = await git_clean_ignored_files(project_path)
    print(f"Cleaned files:\n{clean_output}")

    # Verify some files were removed (gitignored)
    metadata_path = project_path / "src" / "test_app" / "_metadata.py"
    node_modules = project_path / "node_modules"

    assert not metadata_path.exists(), (
        "_metadata.py should be removed by git clean -fdx"
    )
    assert not node_modules.exists(), "node_modules should be removed by git clean -fdx"

    # Step 3: Run build
    print("Step 3: Running build...")
    build_result = await run_cli_async(
        ["build", str(project_path)],
        cwd=project_path,
        timeout=300.0,  # Allow time for dependency installation and build
    )
    assert build_result.returncode == 0, (
        f"build failed: {build_result.stderr}\n{build_result.stdout}"
    )

    # Step 4: Verify build artifacts
    print("Step 4: Verifying build artifacts...")

    build_dir = project_path / ".build"
    assert build_dir.exists(), ".build directory should exist"

    # Find the wheel file
    wheel_files = list(build_dir.glob("*.whl"))
    assert len(wheel_files) == 1, (
        f"Should have exactly one wheel file, found: {wheel_files}"
    )
    wheel_path = wheel_files[0]

    # Step 5: Inspect wheel contents
    print(f"Step 5: Inspecting wheel: {wheel_path.name}...")

    with zipfile.ZipFile(wheel_path, "r") as wheel:
        wheel_contents = wheel.namelist()

        # Check for _metadata.py in the wheel
        metadata_files = [f for f in wheel_contents if f.endswith("_metadata.py")]
        assert len(metadata_files) > 0, (
            f"_metadata.py should be in wheel. Contents: {wheel_contents}"
        )

        # Check for __dist__ directory contents in the wheel
        dist_files = [f for f in wheel_contents if "__dist__" in f]
        # Note: __dist__ should contain built UI assets
        print(f"Files in wheel: {len(wheel_contents)}")
        print(f"_metadata.py files: {metadata_files}")
        print(f"__dist__ files count: {len(dist_files)}")

        # Verify _metadata.py content
        metadata_file = metadata_files[0]
        metadata_content = wheel.read(metadata_file).decode("utf-8")
        assert "app_name" in metadata_content, "_metadata.py should contain app_name"
        assert "api_prefix" in metadata_content, (
            "_metadata.py should contain api_prefix"
        )
        assert "dist_dir" in metadata_content, "_metadata.py should contain dist_dir"

        # __dist__ should have built UI files (index.html, assets, etc.)
        assert len(dist_files) > 0, (
            f"__dist__ should have files in wheel. Contents: {wheel_contents}"
        )

    # Verify requirements.txt was created
    requirements_path = build_dir / "requirements.txt"
    assert requirements_path.exists(), "requirements.txt should exist in .build"
    requirements_content = requirements_path.read_text()
    assert wheel_path.name in requirements_content, (
        "requirements.txt should reference the wheel"
    )

    # Verify app.yml was copied
    app_yml_path = build_dir / "app.yml"
    assert app_yml_path.exists(), "app.yml should be copied to .build"

    print("Test passed: build works after git clean")


async def test_build_produces_complete_wheel(tmp_path: Path) -> None:
    """Test that build produces a complete wheel with all necessary files.

    This test focuses on verifying the wheel structure without the git clean step,
    ensuring the baseline behavior is correct.
    """
    project_path = tmp_path / "complete_wheel_project"
    project_path.mkdir(parents=True, exist_ok=True)

    # Initialize and build project
    print("Initializing and building project...")
    await init_and_build_project(project_path)

    # Find wheel file
    build_dir = project_path / ".build"
    wheel_files = list(build_dir.glob("*.whl"))
    assert len(wheel_files) == 1, (
        f"Should have exactly one wheel file, found: {wheel_files}"
    )
    wheel_path = wheel_files[0]

    print(f"Inspecting wheel: {wheel_path.name}...")

    with zipfile.ZipFile(wheel_path, "r") as wheel:
        wheel_contents = wheel.namelist()

        # Required files that must be in the wheel
        required_patterns = [
            "_metadata.py",  # Generated metadata
            "__dist__/",  # UI distribution directory
            "backend/app.py",  # Backend app
            "backend/router.py",  # Backend router
            "backend/config.py",  # Backend config
        ]

        for pattern in required_patterns:
            matches = [f for f in wheel_contents if pattern in f]
            assert len(matches) > 0, (
                f"Pattern '{pattern}' not found in wheel. Contents: {wheel_contents[:20]}..."
            )

        # __dist__ should have actual UI build artifacts
        dist_files = [
            f
            for f in wheel_contents
            if "__dist__/" in f and not f.endswith("__dist__/")
        ]
        assert len(dist_files) > 0, (
            f"__dist__ should contain UI build files. Found: {dist_files}"
        )

        # Should have index.html in __dist__
        index_files = [f for f in dist_files if "index.html" in f]
        assert len(index_files) > 0, (
            f"__dist__ should contain index.html. Dist files: {dist_files}"
        )

        print(f"Wheel contents verified: {len(wheel_contents)} files")
        print(f"__dist__ files: {len(dist_files)}")

    print("Test passed: wheel contains all required files")
