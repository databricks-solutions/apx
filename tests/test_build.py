import re
import subprocess
import time
from importlib import resources
from pathlib import Path

import pytest
from typer.testing import CliRunner

from apx.__main__ import app
from apx.cli.build import (
    DEFAULT_FALLBACK_VERSION,
    generate_build_version,
    get_base_version,
)

runner: CliRunner = CliRunner()
apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)

# Regex pattern for wheel filename with version and timestamp
# Handles both simple versions (1.2.3+timestamp) and complex versions with local identifiers
# e.g., test_app-0.0.0.post1.dev0+abc123.20260114120000-py3-none-any.whl
WHEEL_FILENAME_PATTERN = re.compile(
    r"test_app-[\d.]+(?:\.post\d+)?(?:\.dev\d+)?(?:\+[a-f0-9]+)?[\+\.]?\d{14}-py3-none-any\.whl"
)


@pytest.fixture
def e2e_init(tmp_path: Path) -> Path:
    """
    Initialize a full e2e project for testing (skipping initial build).
    Returns the path to the initialized project.
    """
    result = runner.invoke(
        app,
        [
            "init",
            str(tmp_path),
            "--assistant",
            "cursor",
            "--layout",
            "basic",
            "--template",
            "essential",
            "--profile",
            "DEFAULT",
            "--name",
            "test-app",
            "--apx-package",
            str(Path(apx_source_dir)),
            "--apx-editable",
            "--skip-build",
        ],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, (
        f"Failed to initialize project. \n output:{result.output} \n error:{result.exception}"
    )
    return tmp_path


class TestGenerateBuildVersion:
    """Tests for the generate_build_version function."""

    def test_generates_version_with_timestamp(self) -> None:
        """Test that generate_build_version creates a version with timestamp suffix."""
        base_version = "1.2.3"
        result = generate_build_version(base_version)

        assert result.startswith("1.2.3+")
        # Check timestamp is 14 digits (YYYYMMDDHHMMSS)
        timestamp = result.split("+")[1]
        assert len(timestamp) == 14
        assert timestamp.isdigit()

    def test_generates_version_with_existing_local_identifier(self) -> None:
        """Test that generate_build_version handles versions with existing local identifier."""
        base_version = "0.0.0.post1.dev0+abc123"
        result = generate_build_version(base_version)

        # Should use '.' separator since '+' is already present
        assert result.startswith("0.0.0.post1.dev0+abc123.")
        # Check timestamp is 14 digits at the end
        timestamp = result.split(".")[-1]
        assert len(timestamp) == 14
        assert timestamp.isdigit()

    def test_different_calls_produce_different_timestamps(self) -> None:
        """Test that consecutive calls produce different timestamps (after waiting)."""
        base_version = "1.0.0"
        result1 = generate_build_version(base_version)
        # Wait a bit to ensure timestamp changes
        time.sleep(1.1)
        result2 = generate_build_version(base_version)

        assert result1 != result2
        assert result1.startswith("1.0.0+")
        assert result2.startswith("1.0.0+")

    def test_preserves_base_version(self) -> None:
        """Test that the base version is preserved in the output."""
        test_cases = ["0.0.0", "1.0.0", "2.5.10", "10.20.30"]
        for base_version in test_cases:
            result = generate_build_version(base_version)
            assert result.startswith(f"{base_version}+")

    def test_preserves_base_version_with_local_identifier(self) -> None:
        """Test that versions with local identifiers are preserved."""
        test_cases = [
            "0.0.0.post1.dev0+abc123",
            "1.0.0+deadbeef",
            "2.5.10.dev0+1234567",
        ]
        for base_version in test_cases:
            result = generate_build_version(base_version)
            # Should start with base version and use '.' separator
            assert result.startswith(f"{base_version}.")


class TestGetBaseVersion:
    """Tests for the get_base_version function."""

    def test_fallback_for_non_project_directory(self, tmp_path: Path) -> None:
        """Test that get_base_version returns fallback for a non-project directory."""
        result = get_base_version(tmp_path)
        assert result == DEFAULT_FALLBACK_VERSION

    def test_returns_version_for_valid_project(self, e2e_init: Path) -> None:
        """Test that get_base_version returns a version for a valid project."""
        result = get_base_version(e2e_init)
        # Should return a valid version (not the fallback since the project has hatch)
        assert result is not None
        assert len(result) > 0
        # Version should match semver-like pattern
        assert re.match(r"^\d+\.\d+\.\d+", result)


class TestBuildE2E:
    """End-to-end tests for the build command."""

    def test_build_creates_wheel_with_versioned_filename(self, e2e_init: Path) -> None:
        """Test that build creates a wheel file with timestamp in filename."""
        # Run build using subprocess to ensure proper module resolution
        result = subprocess.run(
            ["uv", "run", "apx", "build", str(e2e_init)],
            cwd=e2e_init,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, (
            f"Build failed.\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )

        # Check that .build directory exists
        build_dir = e2e_init / ".build"
        assert build_dir.exists()

        # Check that wheel file exists with correct naming pattern
        wheel_files = list(build_dir.glob("*.whl"))
        assert len(wheel_files) == 1, f"Expected 1 wheel file, found {len(wheel_files)}"

        wheel_file = wheel_files[0]
        assert WHEEL_FILENAME_PATTERN.match(wheel_file.name), (
            f"Wheel filename '{wheel_file.name}' doesn't match expected pattern"
        )

        # Check that requirements.txt references the wheel
        requirements_file = build_dir / "requirements.txt"
        assert requirements_file.exists()
        assert wheel_file.name in requirements_file.read_text()

    def test_build_twice_produces_different_wheels(self, e2e_init: Path) -> None:
        """Test that building twice produces wheels with different timestamps."""
        # First build
        result1 = subprocess.run(
            ["uv", "run", "apx", "build", str(e2e_init)],
            cwd=e2e_init,
            capture_output=True,
            text=True,
        )
        assert result1.returncode == 0, (
            f"First build failed.\nstdout: {result1.stdout}\nstderr: {result1.stderr}"
        )

        build_dir = e2e_init / ".build"
        wheel_files1 = list(build_dir.glob("*.whl"))
        assert len(wheel_files1) == 1
        first_wheel_name = wheel_files1[0].name

        # Wait to ensure timestamp changes
        time.sleep(1.1)

        # Second build
        result2 = subprocess.run(
            ["uv", "run", "apx", "build", str(e2e_init)],
            cwd=e2e_init,
            capture_output=True,
            text=True,
        )
        assert result2.returncode == 0, (
            f"Second build failed.\nstdout: {result2.stdout}\nstderr: {result2.stderr}"
        )

        wheel_files2 = list(build_dir.glob("*.whl"))
        assert len(wheel_files2) == 1
        second_wheel_name = wheel_files2[0].name

        # Wheel names should be different (different timestamps)
        assert first_wheel_name != second_wheel_name, (
            f"Expected different wheel names, but both are '{first_wheel_name}'"
        )

        # Both should match the expected pattern
        assert WHEEL_FILENAME_PATTERN.match(first_wheel_name)
        assert WHEEL_FILENAME_PATTERN.match(second_wheel_name)

    def test_build_with_skip_ui_build(self, e2e_init: Path) -> None:
        """Test that build works with --skip-ui-build flag."""
        result = subprocess.run(
            ["uv", "run", "apx", "build", str(e2e_init), "--skip-ui-build"],
            cwd=e2e_init,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, (
            f"Build failed.\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
        assert "Skipping UI build" in result.stdout

        # Wheel should still be created
        build_dir = e2e_init / ".build"
        wheel_files = list(build_dir.glob("*.whl"))
        assert len(wheel_files) == 1
