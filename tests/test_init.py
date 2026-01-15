import subprocess
from importlib import resources
from pathlib import Path
import pytest
from typer.testing import CliRunner

from apx.__main__ import app
from apx.cli.init import is_in_git_repo
from apx.models import Template, Layout
from apx.utils import in_path

runner: CliRunner = CliRunner()
apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)


@pytest.mark.parametrize("template", list(Template))
@pytest.mark.parametrize("layout", list(Layout))
def test_init_no_dependencies(
    tmp_path: Path, template: Template, layout: Layout
) -> None:
    result = runner.invoke(
        app,
        [
            "init",
            str(tmp_path),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
            "--assistant",
            "cursor",
            "--layout",
            layout.value,
            "--template",
            template.value,
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
    assert result.exit_code == 0

    # check that project has src directory
    assert (tmp_path / "src").exists()
    # check that project has src/test-app directory
    assert (tmp_path / "src" / "test_app").exists()
    # check that project has src/test-app/ui directory
    assert (tmp_path / "src" / "test_app" / "ui").exists()
    # check that project has src/test-app/backend directory
    assert (tmp_path / "src" / "test_app" / "backend").exists()


def test_init_e2e(tmp_path: Path) -> None:
    result = runner.invoke(
        app,
        [
            "init",
            str(tmp_path),
            "--assistant",
            "cursor",
            "--layout",
            "sidebar",
            "--template",
            "stateful",
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
        f"Failed to initialize project. \n output:{result.output} \n error:{result.exception} \n stderr:{result.stderr}"
    )

    # make sure __dist__ directory exists
    assert (tmp_path / "src" / "test_app" / "__dist__").exists()
    # make sure .build directory exists
    assert (tmp_path / ".build").exists()
    # make sure .env file exists
    assert (tmp_path / ".env").exists()
    # make sure .env file contains DATABRICKS_CONFIG_PROFILE=DEFAULT
    assert "DATABRICKS_CONFIG_PROFILE" in (tmp_path / ".env").read_text()

    # make sure .env is in .gitignore
    assert ".env" in (tmp_path / ".gitignore").read_text()

    # make sure `dev check` command succeeds
    with in_path(tmp_path):
        result = runner.invoke(
            app,
            ["dev", "check"],
            catch_exceptions=False,
        )
        assert result.exit_code == 0, (
            f"Failed to check project. \n output:{result.output} \n error:{result.exception} \n stderr:{result.stderr}"
        )


def test_is_in_git_repo_returns_true_for_git_repo(tmp_path: Path) -> None:
    """Test that is_in_git_repo returns True for a directory that is a git repo."""
    subprocess.run(["git", "init"], cwd=tmp_path, capture_output=True)
    assert is_in_git_repo(tmp_path) is True


def test_is_in_git_repo_returns_true_for_subdirectory_of_git_repo(
    tmp_path: Path,
) -> None:
    """Test that is_in_git_repo returns True for a subdirectory of a git repo."""
    subprocess.run(["git", "init"], cwd=tmp_path, capture_output=True)
    subdir = tmp_path / "subdir"
    subdir.mkdir()
    assert is_in_git_repo(subdir) is True


def test_is_in_git_repo_returns_false_for_non_git_directory(tmp_path: Path) -> None:
    """Test that is_in_git_repo returns False for a directory that is not a git repo."""
    assert is_in_git_repo(tmp_path) is False


def test_init_skips_git_init_when_already_in_repo(tmp_path: Path) -> None:
    """Test that apx init skips git initialization when the directory is already in a git repo."""
    # Initialize git repo first
    subprocess.run(["git", "init"], cwd=tmp_path, capture_output=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=tmp_path,
        capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test User"], cwd=tmp_path, capture_output=True
    )

    result = runner.invoke(
        app,
        [
            "init",
            str(tmp_path),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
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
        ],
        catch_exceptions=False,
    )
    assert result.exit_code == 0
    assert "Skipping git init" in result.output


def test_init_skips_git_init_when_in_parent_repo(tmp_path: Path) -> None:
    """Test that apx init skips git initialization when initializing in a subdirectory of a git repo."""
    # Initialize git repo in parent directory
    subprocess.run(["git", "init"], cwd=tmp_path, capture_output=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=tmp_path,
        capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test User"], cwd=tmp_path, capture_output=True
    )

    # Create subdirectory for the new project
    subdir = tmp_path / "my-project"
    subdir.mkdir()

    result = runner.invoke(
        app,
        [
            "init",
            str(subdir),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
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
        ],
        catch_exceptions=False,
    )
    assert result.exit_code == 0
    assert "Skipping git init" in result.output
