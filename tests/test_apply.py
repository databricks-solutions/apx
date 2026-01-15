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


def test_apply_single_file_from_base(tmp_path: Path) -> None:
    """Test applying a single file from the base template."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify vite.config.ts exists
    vite_config_path = tmp_path / "vite.config.ts"
    assert vite_config_path.exists()

    # Read original vite.config.ts content
    original_content = vite_config_path.read_text()
    assert "apxPlugin()" in original_content

    # Modify the file to simulate user changes
    modified_content = original_content.replace("apxPlugin()", "// modified content")
    vite_config_path.write_text(modified_content)

    # Verify the modification
    assert "// modified content" in vite_config_path.read_text()
    assert "apxPlugin()" not in vite_config_path.read_text()

    # Apply the single file from base template with --force
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "base", "--file", "vite.config.ts", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply single file. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify the file was restored to the original template version
    restored_content = vite_config_path.read_text()
    assert "apxPlugin()" in restored_content
    assert "// modified content" not in restored_content


def test_apply_single_file_with_path_substitution(tmp_path: Path) -> None:
    """Test applying a single file with 'base' to app_slug path substitution."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify the backend router.py exists
    router_path = tmp_path / "src" / "test_app" / "backend" / "router.py"
    assert router_path.exists()

    # Read original router.py content
    original_content = router_path.read_text()

    # Modify the file
    modified_content = "# This is a modified router\n" + original_content
    router_path.write_text(modified_content)

    # Apply the single file from base template
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            [
                "dev",
                "apply",
                "base",
                "--file",
                "src/base/backend/router.py",
                "--force",
            ],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply single file with path substitution. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output
        # Check that the output shows the correct target path
        assert "test_app/backend/router.py" in result.output

    # Verify the file was restored
    restored_content = router_path.read_text()
    assert "# This is a modified router" not in restored_content
    assert original_content == restored_content


def test_apply_single_file_with_confirmation_prompt(tmp_path: Path) -> None:
    """Test that single file apply prompts for confirmation when file exists."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify vite.config.ts exists
    vite_config_path = tmp_path / "vite.config.ts"
    assert vite_config_path.exists()

    # Modify the file
    original_content = vite_config_path.read_text()
    modified_content = "// modified\n" + original_content
    vite_config_path.write_text(modified_content)

    # Apply without --force, cancel the prompt
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "base", "--file", "vite.config.ts"],
            input="n\n",  # Answer 'no' to the prompt
            catch_exceptions=False,
        )
        assert result.exit_code == 0
        assert "will be overwritten" in result.output
        assert "cancelled" in result.output.lower()

    # Verify the file was NOT restored (user cancelled)
    assert "// modified" in vite_config_path.read_text()


def test_apply_single_file_nonexistent(tmp_path: Path) -> None:
    """Test that apply fails gracefully when trying to apply a nonexistent file."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Try to apply a non-existent file
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "base", "--file", "nonexistent-file.ts", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 1
        assert "not found" in result.output.lower()


def test_apply_single_file_from_essential(tmp_path: Path) -> None:
    """Test applying a single file using 'essential' as an alias for 'base'."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Verify vite.config.ts exists
    vite_config_path = tmp_path / "vite.config.ts"
    assert vite_config_path.exists()

    # Read original vite.config.ts content
    original_content = vite_config_path.read_text()

    # Modify the file
    modified_content = original_content.replace("apxPlugin()", "// modified content")
    vite_config_path.write_text(modified_content)

    # Apply the single file from essential template (should work as alias for base)
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "essential", "--file", "vite.config.ts", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply single file from essential. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify the file was restored
    restored_content = vite_config_path.read_text()
    assert "apxPlugin()" in restored_content
    assert "// modified content" not in restored_content


def test_apply_essential_without_file_flag_succeeds(tmp_path: Path) -> None:
    """Test that applying 'essential' without --file flag works (applies entire essential template)."""
    # Initialize with essential template
    initialize_project(tmp_path, template="essential", layout="basic")

    # Modify multiple files to verify they get restored
    vite_config_path = tmp_path / "vite.config.ts"
    vite_config_path.write_text("// modified vite")

    router_path = tmp_path / "src" / "test_app" / "backend" / "router.py"
    router_path.write_text("# modified router")

    # Apply 'essential' template (should restore all base template files)
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "apply", "essential", "--force"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to apply essential template. \n output:{result.output} \n error:{result.exception}"
        )
        assert "applied successfully" in result.output

    # Verify both files were restored
    assert "apxPlugin()" in vite_config_path.read_text()
    assert "// modified vite" not in vite_config_path.read_text()
    assert "# modified router" not in router_path.read_text()
    # Verify router has expected content
    assert "APIRouter" in router_path.read_text()
