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

    # Verify all generated UI component filenames are lowercase (regression test)
    ui_components_apx_path = app_path / "src" / app_slug / "ui" / "components" / "apx"
    console.print(f"[dim]Checking UI components naming: {ui_components_apx_path}[/dim]")
    assert ui_components_apx_path.exists(), "ui/components/apx directory should exist"
    assert ui_components_apx_path.is_dir(), "ui/components/apx should be a directory"

    component_files = sorted(
        [
            p.name
            for p in ui_components_apx_path.iterdir()
            if p.is_file() and not p.name.startswith(".")
        ]
    )
    assert component_files, "ui/components/apx should contain at least one file"
    non_lowercase = [name for name in component_files if name != name.lower()]
    assert not non_lowercase, (
        "All files in ui/components/apx must be named in lowercase, "
        f"but found: {non_lowercase}"
    )

    # Check that package.json was created
    console.print(f"[dim]Checking package.json: {app_path / 'package.json'}[/dim]")
    assert (app_path / "package.json").exists(), "package.json should exist"

    # Check that pyproject.toml was created
    console.print(f"[dim]Checking pyproject.toml: {app_path / 'pyproject.toml'}[/dim]")
    assert (app_path / "pyproject.toml").exists(), "pyproject.toml should exist"

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

    console.print(f"[green]✓ Test passed for {template.value}/{layout.value}[/green]")


def test_components_json_app_slug_replacement(
    node_modules_dir: Path,
    tmp_path: Path,
) -> None:
    """
    Test that components.json is generated with the correct app_slug replacement.

    The components.json.jinja2 template uses {{app_slug}} in the CSS path:
    "css": "src/{{app_slug}}/ui/styles/globals.css"

    This test verifies that the generated components.json file contains the actual
    app_slug (e.g., "my_test_app") and not the literal string "base".
    """
    import json

    # Use a specific app name with dashes to test slug conversion
    test_app_name = "my-test-app"
    expected_app_slug = "my_test_app"  # dashes should be converted to underscores

    console.print(
        f"\n[bold]Testing components.json generation for {test_app_name}[/bold]"
    )

    app_path = tmp_path
    app_path.mkdir(parents=True, exist_ok=True)

    # Copy node_modules to speed up the test
    console.print("[dim]Copying node_modules...[/dim]")
    shutil.copytree(node_modules_dir, app_path / "node_modules")

    # Mock prompts to avoid interactive input
    def mock_prompt_ask(*args, **kwargs):
        return ""

    def mock_confirm_ask(*args, **kwargs):
        return False

    with (
        patch("apx.cli.init.Prompt.ask", side_effect=mock_prompt_ask),
        patch("apx.cli.init.Confirm.ask", side_effect=mock_confirm_ask),
    ):
        console.print(f"[dim]Running apx init for {test_app_name}...[/dim]")
        result = runner.invoke(
            app,
            [
                "init",
                str(app_path),
                "--name",
                test_app_name,
                "--template",
                Template.essential.value,
                "--layout",
                Layout.basic.value,
                "--apx-package",
                apx_source_dir,
                "-p",
                "DEFAULT",
            ],
        )

        console.print(f"[dim]Init command exit code: {result.exit_code}[/dim]")
        if result.stdout:
            console.print(f"[dim]stdout: {result.stdout}[/dim]")
        if result.stderr:
            console.print(f"[dim]stderr: {result.stderr}[/dim]")

        assert result.exit_code == 0, (
            f"init should exit with code 0, got {result.exit_code}\n"
            f"Output: {result.stdout}"
        )

    # Verify that components.json was created
    components_json_path = app_path / "components.json"
    console.print(f"[dim]Checking components.json: {components_json_path}[/dim]")
    assert components_json_path.exists(), "components.json should exist"

    # Read and parse the components.json file
    with open(components_json_path, "r") as f:
        components_config = json.load(f)

    console.print(
        f"[dim]components.json content: {json.dumps(components_config, indent=2)}[/dim]"
    )

    # Verify the CSS path contains the actual app_slug, not "base"
    css_path = components_config.get("tailwind", {}).get("css", "")
    console.print(f"[dim]CSS path from components.json: {css_path}[/dim]")

    expected_css_path = f"src/{expected_app_slug}/ui/styles/globals.css"
    console.print(f"[dim]Expected CSS path: {expected_css_path}[/dim]")

    assert css_path == expected_css_path, (
        f"CSS path should be '{expected_css_path}', but got '{css_path}'. "
        f"The {{{{app_slug}}}} template variable should be replaced with the actual app slug, "
        f"not the literal string 'base'."
    )

    # Also verify that "base" is NOT in the CSS path
    assert "base" not in css_path, (
        f"CSS path should not contain 'base', but got '{css_path}'. "
        f"The template should replace {{{{app_slug}}}} with '{expected_app_slug}'."
    )

    console.print(
        f"[green]✓ components.json correctly uses app_slug '{expected_app_slug}' in CSS path[/green]"
    )
