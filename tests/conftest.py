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
    import os
    import sys

    # Set APX_DEV_PATH environment variable for editable installation
    os.environ["APX_DEV_PATH"] = str(Path(apx_source_dir))

    try:
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
                "--skip-build",
            ]
        )
        assert exit_code == 0
        sys.path.insert(0, str(tmp_path / "src"))
        return tmp_path
    finally:
        # Clean up environment variable after initialization
        os.environ.pop("APX_DEV_PATH", None)


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
