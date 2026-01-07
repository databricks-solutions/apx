from importlib import resources
from pathlib import Path

from typer.testing import CliRunner

from apx.__main__ import app
from apx.utils import in_path

runner: CliRunner = CliRunner()
apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)


def initialize_project(
    tmp_path: Path,
    template: str = "essential",
    layout: str = "basic",
    assistant: str = "cursor",
) -> None:
    """Helper function to initialize a project without dependencies."""
    result = runner.invoke(
        app,
        [
            "init",
            str(tmp_path),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
            "--assistant",
            assistant,
            "--layout",
            layout,
            "--template",
            template,
            "--profile",
            "DEFAULT",
            "--name",
            "test-app",
            "--apx-package",
            str(Path(apx_source_dir)),
            "--apx-editable",
        ],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, (
        f"Failed to initialize project. \n output:{result.output} \n error:{result.exception}"
    )


def test_apply_stateful_addon(tmp_path: Path) -> None:
    """Test applying stateful addon to an essential template project."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify base files exist
    assert (tmp_path / "src" / "test_app" / "backend" / "app.py").exists()
    assert (tmp_path / "src" / "test_app" / "backend" / "runtime.py").exists()

    # Read original runtime.py to verify it gets updated
    original_runtime = (
        tmp_path / "src" / "test_app" / "backend" / "runtime.py"
    ).read_text()

    # Apply stateful addon with --force to avoid prompt
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "stateful", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply stateful addon. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify stateful-specific files are present
    assert (tmp_path / "app.yml").exists()
    assert (tmp_path / "databricks.yml").exists()

    # Verify databricks.yml contains stateful-specific content (database_instances)
    databricks_yml = (tmp_path / "databricks.yml").read_text()
    assert "database_instances" in databricks_yml, (
        "databricks.yml should contain database_instances for stateful"
    )

    # Verify runtime.py was updated (stateful has more complex runtime)
    updated_runtime = (
        tmp_path / "src" / "test_app" / "backend" / "runtime.py"
    ).read_text()
    assert len(updated_runtime) > len(original_runtime), (
        "runtime.py should be expanded with stateful content"
    )


def test_apply_sidebar_layout(tmp_path: Path) -> None:
    """Test applying sidebar layout addon to a basic layout project."""
    # Initialize with basic layout
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify base UI routes exist
    assert (tmp_path / "src" / "test_app" / "ui" / "routes" / "__root.tsx").exists()
    assert (tmp_path / "src" / "test_app" / "ui" / "routes" / "index.tsx").exists()

    # Verify sidebar-specific files don't exist yet
    assert not (
        tmp_path
        / "src"
        / "test_app"
        / "ui"
        / "components"
        / "apx"
        / "sidebar-layout.tsx"
    ).exists()

    # Apply sidebar addon with --force
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "sidebar", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply sidebar addon. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify sidebar-specific files are now present
    assert (
        tmp_path
        / "src"
        / "test_app"
        / "ui"
        / "components"
        / "apx"
        / "sidebar-layout.tsx"
    ).exists()
    assert (
        tmp_path
        / "src"
        / "test_app"
        / "ui"
        / "components"
        / "apx"
        / "sidebar-user-footer.tsx"
    ).exists()
    assert (
        tmp_path / "src" / "test_app" / "ui" / "routes" / "_sidebar" / "route.tsx"
    ).exists()
    assert (
        tmp_path / "src" / "test_app" / "ui" / "routes" / "_sidebar" / "profile.tsx"
    ).exists()


def test_apply_assistant_addon(tmp_path: Path) -> None:
    """Test applying assistant addon (cursor)."""
    # Initialize with no assistant
    initialize_project(
        tmp_path, template="essential", layout="basic", assistant="cursor"
    )

    # Apply claude assistant addon with --force
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "claude", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply claude addon. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify claude-specific files are now present
    assert (tmp_path / "CLAUDE.md").exists()


def test_apply_with_conflict_prompt_cancelled(tmp_path: Path) -> None:
    """Test that apply prompts for confirmation when files conflict and user cancels."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Apply stateful addon without --force, provide 'n' to cancel
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "stateful"],
            input="n\n",  # Answer 'no' to the prompt
            catch_exceptions=False,
        )
        # Should exit cleanly with code 0 when user cancels
        assert result.exit_code == 0
        assert "will be overwritten" in result.output
        assert "cancelled" in result.output.lower()


def test_apply_with_conflict_prompt_accepted(tmp_path: Path) -> None:
    """Test that apply proceeds when user confirms overwriting files."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Apply stateful addon without --force, provide 'y' to accept
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "stateful"],
            input="y\n",  # Answer 'yes' to the prompt
            catch_exceptions=False,
        )
        assert result.exit_code == 0
        assert "will be overwritten" in result.output
        assert "applied successfully" in result.output


def test_apply_invalid_addon(tmp_path: Path) -> None:
    """Test that apply fails gracefully with invalid addon name."""
    # Initialize project
    initialize_project(tmp_path, template="essential", layout="basic")

    # Try to apply non-existent addon
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "nonexistent-addon", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 1
        assert "Invalid addon" in result.output


def test_apply_outside_project(tmp_path: Path) -> None:
    """Test that apply fails gracefully when not in a project directory."""
    # Don't initialize a project, just try to apply an addon
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "stateful", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 1
        assert "Failed to read project metadata" in result.output
