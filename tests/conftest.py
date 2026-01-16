from importlib import resources
from pathlib import Path
from typing import Callable

import pytest
from apx._core import run_cli
from _pytest.capture import CaptureFixture
from pydantic import BaseModel


class RunApxResult(BaseModel):
    code: int
    out: str
    err: str


Args = list[str]

apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)


@pytest.fixture
def e2e_init(tmp_path: Path) -> Path:
    """
    Initialize a full e2e project for testing (skipping initial build).
    Returns the path to the initialized project.
    """

    exit_code = run_cli(
        [
            "apx",
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
        ]
    )
    assert exit_code == 0
    import sys

    sys.path.insert(0, str(tmp_path / "src"))
    return tmp_path


@pytest.fixture
def run_apx(capfd: CaptureFixture[str]) -> Callable[[Args], RunApxResult]:
    def _run(args: Args) -> RunApxResult:
        full_args = ["apx"] + args
        code: int = run_cli(full_args)
        out: str
        err: str
        out, err = capfd.readouterr()
        return RunApxResult(code=code, out=out, err=err)

    return _run


ApxFixture = Callable[[Args], RunApxResult]
