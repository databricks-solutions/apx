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
)
from apx.__main__ import app
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
    console.print(f"[dim]Temp path: {tmp_path}[/dim]")

    # init bun project
    console.print("[dim]Running bun init...[/dim]")
    try:
        result = subprocess.run(
            ["bun", "init", "-m", "-y"],
            cwd=tmp_path,
            env=os.environ,
            check=True,
            capture_output=True,
            text=True,
        )
        console.print(f"[dim]Bun init stdout: {result.stdout}[/dim]")
        if result.stderr:
            console.print(f"[dim]Bun init stderr: {result.stderr}[/dim]")
    except subprocess.CalledProcessError as e:
        console.print(
            f"[red]ERROR: Bun init failed with exit code {e.returncode}[/red]"
        )
        console.print(f"[red]stdout: {e.stdout}[/red]")
        console.print(f"[red]stderr: {e.stderr}[/red]")
        raise

    # add bun dependencies to populate the node_modules directory
    console.print("[dim]Adding bun dependencies...[/dim]")
    add_bun_dependencies(tmp_path)
    console.print("[dim]Adding bun dev dependencies...[/dim]")
    add_bun_dev_dependencies(tmp_path)

    time_end = time.perf_counter()
    console.print(
        f"Node modules directory created in {time_end - time_start:.2f} seconds"
    )

    # Verify node_modules was created
    node_modules_path = tmp_path / "node_modules"
    if node_modules_path.exists():
        console.print(f"[dim]node_modules exists at {node_modules_path}[/dim]")
        # Count directories in node_modules
        try:
            num_packages = len([d for d in node_modules_path.iterdir() if d.is_dir()])
            console.print(f"[dim]Found {num_packages} packages in node_modules[/dim]")
        except Exception as e:
            console.print(f"[yellow]Warning: Could not count packages: {e}[/yellow]")
    else:
        console.print(
            f"[red]WARNING: node_modules does not exist at {node_modules_path}[/red]"
        )

    yield node_modules_path


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
    console.print(f"\n[bold]Testing {template.value}/{layout.value}[/bold]")
    console.print(f"[dim]Test app name: {test_app_name}[/dim]")

    app_path = tmp_path
    console.print(f"[dim]App path: {app_path}[/dim]")
    app_path.mkdir(parents=True, exist_ok=True)

    # copy the node_modules directory to the app path
    console.print(
        f"[dim]Copying node_modules from {node_modules_dir} to {app_path / 'node_modules'}[/dim]"
    )
    try:
        shutil.copytree(node_modules_dir, app_path / "node_modules")
        console.print("[dim]node_modules copied successfully[/dim]")
    except Exception as e:
        console.print(f"[red]ERROR copying node_modules: {e}[/red]")
        raise

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
        # Run init using CliRunner to capture all output
        console.print("[dim]Running apx init with args:[/dim]")
        console.print(f"[dim]  path: {app_path}[/dim]")
        console.print(f"[dim]  name: {test_app_name}[/dim]")
        console.print(f"[dim]  template: {template.value}[/dim]")
        console.print(f"[dim]  layout: {layout.value}[/dim]")
        console.print(f"[dim]  apx-package: {apx_source_dir}[/dim]")

        result = runner.invoke(
            app,
            [
                "init",
                str(app_path),
                "--name",
                test_app_name,
                "--template",
                template.value,
                "--layout",
                layout.value,
                "--apx-package",
                apx_source_dir,
            ],
        )

        # Print captured output for debugging
        console.print(f"[dim]Init command exit code: {result.exit_code}[/dim]")
        if result.stdout:
            console.print("\n[dim]Captured stdout:[/dim]")
            console.print(result.stdout)
        if result.stderr:
            console.print("\n[dim]Captured stderr:[/dim]")
            console.print(result.stderr)
        if result.exception:
            console.print("\n[red]Exception occurred:[/red]")
            console.print(f"[red]{result.exception}[/red]")
            import traceback

            console.print(
                f"[red]{''.join(traceback.format_exception(type(result.exception), result.exception, result.exception.__traceback__))}[/red]"
            )

        # Assert successful execution
        assert result.exit_code == 0, (
            f"init should exit with code 0, got {result.exit_code}\n"
            f"Output: {result.stdout}\n"
            f"Error: {result.stderr}\n"
            f"Exception: {result.exception if result.exception else 'None'}"
        )

    # Verify that key directories and files were created
    console.print("[dim]Verifying created files and directories...[/dim]")
    app_slug = test_app_name.replace("-", "_")
    console.print(f"[dim]App slug: {app_slug}[/dim]")

    # Check basic structure
    console.print(f"[dim]Checking src directory: {app_path / 'src'}[/dim]")
    assert (app_path / "src").exists(), "src directory should exist"

    console.print(
        f"[dim]Checking app module directory: {app_path / 'src' / app_slug}[/dim]"
    )
    assert (app_path / "src" / app_slug).exists(), "app module directory should exist"

    console.print(
        f"[dim]Checking backend directory: {app_path / 'src' / app_slug / 'backend'}[/dim]"
    )
    assert (app_path / "src" / app_slug / "backend").exists(), (
        "backend directory should exist"
    )

    console.print(
        f"[dim]Checking ui directory: {app_path / 'src' / app_slug / 'ui'}[/dim]"
    )
    assert (app_path / "src" / app_slug / "ui").exists(), "ui directory should exist"

    # Check that package.json was created
    console.print(f"[dim]Checking package.json: {app_path / 'package.json'}[/dim]")
    assert (app_path / "package.json").exists(), "package.json should exist"

    # Check that pyproject.toml was created
    console.print(f"[dim]Checking pyproject.toml: {app_path / 'pyproject.toml'}[/dim]")
    assert (app_path / "pyproject.toml").exists(), "pyproject.toml should exist"

    # Check build directory was created
    console.print(f"[dim]Checking .build directory: {app_path / '.build'}[/dim]")
    assert (app_path / ".build").exists(), ".build directory should exist"

    # Verify template-specific files
    if template == Template.stateful:
        # Check that stateful-specific backend files exist
        backend_path = app_path / "src" / app_slug / "backend"
        console.print(
            f"[dim]Checking stateful template runtime.py: {backend_path / 'runtime.py'}[/dim]"
        )
        assert (backend_path / "runtime.py").exists(), (
            "runtime.py should exist for stateful template"
        )

    # Verify layout-specific files
    if layout == Layout.sidebar:
        # Check that sidebar-specific components exist
        ui_components_path = app_path / "src" / app_slug / "ui" / "components" / "apx"
        console.print(
            f"[dim]Checking sidebar layout component: {ui_components_path / 'sidebar-layout.tsx'}[/dim]"
        )
        assert (ui_components_path / "sidebar-layout.tsx").exists(), (
            "sidebar-layout.tsx should exist for sidebar layout"
        )

    # Verify that .env file was created (but without DATABRICKS_CONFIG_PROFILE since profile=None)
    console.print(f"[dim]Checking .env file: {app_path / '.env'}[/dim]")
    if (app_path / ".env").exists():
        env_contents = (app_path / ".env").read_text()
        # Ensure DATABRICKS_CONFIG_PROFILE is not in the file since profile=None
        assert "DATABRICKS_CONFIG_PROFILE" not in env_contents, (
            "DATABRICKS_CONFIG_PROFILE should not be set when profile=None"
        )
        console.print("[dim].env file verified (no DATABRICKS_CONFIG_PROFILE)[/dim]")

    # Verify that the build completed successfully
    # The build directory should contain a wheel file and requirements.txt
    build_dir = app_path / ".build"
    console.print(f"[dim]Verifying build artifacts in {build_dir}...[/dim]")
    wheel_files = list(build_dir.glob("*.whl"))
    console.print(
        f"[dim]Found {len(wheel_files)} wheel file(s): {[f.name for f in wheel_files]}[/dim]"
    )
    assert len(wheel_files) > 0, (
        f"At least one wheel file should exist in .build directory for {template}/{layout}"
    )

    console.print(
        f"[dim]Checking requirements.txt: {build_dir / 'requirements.txt'}[/dim]"
    )
    assert (build_dir / "requirements.txt").exists(), (
        f"requirements.txt should exist in .build directory for {template}/{layout}"
    )

    # Verify requirements.txt contains the wheel file name
    requirements_content = (build_dir / "requirements.txt").read_text()
    console.print(f"[dim]requirements.txt content:\n{requirements_content}[/dim]")
    assert wheel_files[0].name in requirements_content, (
        "requirements.txt should reference the wheel file"
    )

    console.print(f"[green]✓ Test passed for {template.value}/{layout.value}[/green]")
