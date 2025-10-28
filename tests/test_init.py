import pytest
import os
from pathlib import Path
from unittest.mock import patch
from typer.testing import CliRunner
from apx.__main__ import init
from collections.abc import Generator

runner: CliRunner = CliRunner()


@pytest.fixture(scope="session", autouse=True)
def common_caches(
    tmp_path_factory: pytest.TempPathFactory,
) -> Generator[tuple[Path, Path], None, None]:
    """
    Create a shared UV cache directory for all tests to speed up package installations.
    Sets UV_CACHE_DIR environment variable to use this shared cache.
    """
    # Create a shared temp directory for UV cache
    cache_dir = tmp_path_factory.mktemp("uv_cache")
    bun_cache_dir = tmp_path_factory.mktemp("bun_cache")

    os.environ["UV_CACHE_DIR"] = str(cache_dir)
    os.environ["BUN_CACHE_DIR"] = str(bun_cache_dir)

    yield cache_dir, bun_cache_dir


@pytest.mark.parametrize(
    "template,layout",
    [
        ("essential", "basic"),
        ("essential", "sidebar"),
        ("stateful", "basic"),
        ("stateful", "sidebar"),
    ],
)
def test_init_and_build_combinations(
    tmp_path: Path,
    template: str,
    layout: str,
):
    """
    Test that init works with different template and layout combinations.
    Verifies that build completes successfully for each combination.
    Uses a shared UV cache directory to speed up package installations.
    """
    # Create a unique directory for this test case
    test_app_name = f"test-app-{template}-{layout}"
    app_path = tmp_path / test_app_name
    app_path.mkdir(parents=True, exist_ok=True)

    # Mock the Prompt.ask to return empty string (to skip profile setup)
    # and Confirm.ask to return False (to skip assistant setup when profile is skipped)
    def mock_prompt_ask(*args, **kwargs):
        # Return empty string for profile prompt
        return ""

    def mock_confirm_ask(*args, **kwargs):
        # Return False for assistant confirmation
        return False

    # Patch the prompts to avoid interactive input during tests
    with (
        patch("apx.__main__.Prompt.ask", side_effect=mock_prompt_ask),
        patch("apx.__main__.Confirm.ask", side_effect=mock_confirm_ask),
    ):
        # Run init with all parameters passed by name (typer requirement)
        init(
            app_path=app_path,
            app_name=test_app_name,
            template=template,  # type: ignore
            profile=None,  # This will trigger prompting, but we mock it to return ""
            assistant=None,  # This will trigger prompting, but we mock it to return False
            layout=layout,  # type: ignore
            version=None,
        )

    # Verify that key directories and files were created
    app_slug = test_app_name.replace("-", "_")

    # Check basic structure
    assert (app_path / "src").exists(), "src directory should exist"
    assert (app_path / "src" / app_slug).exists(), "app module directory should exist"
    assert (app_path / "src" / app_slug / "backend").exists(), (
        "backend directory should exist"
    )
    assert (app_path / "src" / app_slug / "ui").exists(), "ui directory should exist"

    # Check that package.json was created
    assert (app_path / "package.json").exists(), "package.json should exist"

    # Check that pyproject.toml was created
    assert (app_path / "pyproject.toml").exists(), "pyproject.toml should exist"

    # Check build directory was created
    assert (app_path / ".build").exists(), ".build directory should exist"

    # Verify template-specific files
    if template == "stateful":
        # Check that stateful-specific backend files exist
        backend_path = app_path / "src" / app_slug / "backend"
        assert (backend_path / "runtime.py").exists(), (
            "runtime.py should exist for stateful template"
        )

    # Verify layout-specific files
    if layout == "sidebar":
        # Check that sidebar-specific components exist
        ui_components_path = app_path / "src" / app_slug / "ui" / "components" / "apx"
        assert (ui_components_path / "SidebarLayout.tsx").exists(), (
            "SidebarLayout.tsx should exist for sidebar layout"
        )

    # Verify that .env file was created (but without DATABRICKS_CONFIG_PROFILE since profile=None)
    if (app_path / ".env").exists():
        env_contents = (app_path / ".env").read_text()
        # Ensure DATABRICKS_CONFIG_PROFILE is not in the file since profile=None
        assert "DATABRICKS_CONFIG_PROFILE" not in env_contents, (
            "DATABRICKS_CONFIG_PROFILE should not be set when profile=None"
        )

    # Verify that the build completed successfully
    # The build directory should contain a wheel file and requirements.txt
    build_dir = app_path / ".build"
    wheel_files = list(build_dir.glob("*.whl"))
    assert len(wheel_files) > 0, (
        f"At least one wheel file should exist in .build directory for {template}/{layout}"
    )
    assert (build_dir / "requirements.txt").exists(), (
        f"requirements.txt should exist in .build directory for {template}/{layout}"
    )

    # Verify requirements.txt contains the wheel file name
    requirements_content = (build_dir / "requirements.txt").read_text()
    assert wheel_files[0].name in requirements_content, (
        "requirements.txt should reference the wheel file"
    )
