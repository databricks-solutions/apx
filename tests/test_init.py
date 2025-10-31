from importlib import resources
import shutil
import time
import pytest
import os
from pathlib import Path
from unittest.mock import patch
from typer.testing import CliRunner
from apx.cli.init import (
    Layout,
    Template,
    add_bun_dependencies,
    add_bun_dev_dependencies,
    init,
)
from collections.abc import Generator
import subprocess
from apx.utils import console

runner: CliRunner = CliRunner()
apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)


@pytest.fixture(scope="session", autouse=True)
def node_modules_dir(
    tmp_path_factory: pytest.TempPathFactory,
) -> Generator[Path, None, None]:
    """
    Create a node_modules directory for all tests to speed up package installations.
    """

    time_start = time.perf_counter()
    console.print("Creating node_modules directory...")
    tmp_path = tmp_path_factory.mktemp("node_modules")

    # init bun project
    subprocess.run(
        ["bun", "init", "-m", "-y"], cwd=tmp_path, env=os.environ, check=True
    )

    # add bun dependencies to populate the node_modules directory
    add_bun_dependencies(tmp_path)
    add_bun_dev_dependencies(tmp_path)

    time_end = time.perf_counter()
    console.print(f"Node modules directory created in {time_end - time_start} seconds")

    yield tmp_path / "node_modules"


@pytest.mark.parametrize(
    "template,layout",
    [
        (Template.essential, Layout.basic),
        (Template.essential, Layout.sidebar),
        (Template.stateful, Layout.basic),
        (Template.stateful, Layout.sidebar),
    ],
)
def test_init_and_build_combinations(
    node_modules_dir: Path,
    tmp_path: Path,
    template: Template,
    layout: Layout,
):
    """
    Test that init works with different template and layout combinations.
    Verifies that build completes successfully for each combination.
    Uses a shared UV cache directory to speed up package installations.
    """
    # Create a unique directory for this test case
    test_app_name = f"test-app-{template.value}-{layout.value}"
    app_path = tmp_path
    app_path.mkdir(parents=True, exist_ok=True)
    # copy the node_modules directory to the app path
    shutil.copytree(node_modules_dir, app_path / "node_modules")

    # Mock the Prompt.ask to return empty string (to skip profile setup)
    # and Confirm.ask to return False (to skip assistant setup when profile is skipped)
    def mock_prompt_ask(
        *args,  # pyright:ignore[reportUnusedParameter,reportMissingParameterType, reportUnknownParameterType]
        **kwargs,  # pyright:ignore[reportUnusedParameter,reportMissingParameterType, reportUnknownParameterType]
    ):
        # Return empty string for profile prompt
        return ""

    def mock_confirm_ask(
        *args,  # pyright:ignore[reportUnusedParameter,reportMissingParameterType, reportUnknownParameterType]
        **kwargs,  # pyright:ignore[reportUnusedParameter,reportMissingParameterType, reportUnknownParameterType]
    ):
        # Return False for assistant confirmation
        return False

    # Patch the prompts to avoid interactive input during tests
    with (
        patch("apx.cli.init.Prompt.ask", side_effect=mock_prompt_ask),
        patch("apx.cli.init.Confirm.ask", side_effect=mock_confirm_ask),
    ):
        # Run init with all parameters passed by name (typer requirement)
        result = init(
            app_path=app_path,
            app_name=test_app_name,
            template=template,
            assistant=None,
            layout=layout,
            apx_package=apx_source_dir,
        )
        if result:
            assert result.exit_code == 0, "init should exit with code 0"

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
    if template == Template.stateful:
        # Check that stateful-specific backend files exist
        backend_path = app_path / "src" / app_slug / "backend"
        assert (backend_path / "runtime.py").exists(), (
            "runtime.py should exist for stateful template"
        )

    # Verify layout-specific files
    if layout == Layout.sidebar:
        # Check that sidebar-specific components exist
        ui_components_path = app_path / "src" / app_slug / "ui" / "components" / "apx"
        assert (ui_components_path / "sidebar-layout.tsx").exists(), (
            "sidebar-layout.tsx should exist for sidebar layout"
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
