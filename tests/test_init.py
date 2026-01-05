from importlib import resources
from pathlib import Path
import pytest
from typer.testing import CliRunner

from apx.__main__ import app
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
