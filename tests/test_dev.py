from dataclasses import dataclass
import os
from pathlib import Path
import subprocess
import time


@dataclass
class RunApxResult:
    code: int
    out: str
    err: str


def run_apx_subprocess(args: list[str], cwd: Path) -> RunApxResult:
    env = os.environ.copy()
    result = subprocess.run(
        ["uv", "run", "apx", *args],
        cwd=str(cwd),
        env=env,
        capture_output=True,
        text=True,
    )
    return RunApxResult(code=result.returncode, out=result.stdout, err=result.stderr)


def start_apx_logs_follow(args: list[str], cwd: Path) -> subprocess.Popen[str]:
    env = os.environ.copy()
    env["APX_LOG"] = "debug"
    env["APX_COLLECT_LOGS"] = "1"
    return subprocess.Popen(
        ["uv", "run", "apx", *args],
        cwd=str(cwd),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def test_dev_server_start_stop_with_logs(e2e_init: Path) -> None:
    try:
        print(f"Starting dev server in {e2e_init}")
        start_result = run_apx_subprocess(["dev", "start"], e2e_init)
        assert start_result.code == 0

        # check the server logs
        print(f"Checking server logs in {e2e_init}")
        logs_result = run_apx_subprocess(["dev", "logs"], e2e_init)
        print(
            f"logs result: {logs_result} with error: {logs_result.err} and output: {logs_result.out}"
        )

        # should contain:
        # should contain at least some lines from process manager and from server
        assert "dev::process" in logs_result.out
        assert "dev::server" in logs_result.out

        follow_process = start_apx_logs_follow(
            ["dev", "logs", "--follow", str(e2e_init)], e2e_init
        )
        time.sleep(0.2)
        stop_result = run_apx_subprocess(["dev", "stop"], e2e_init)
        assert stop_result.code == 0
        try:
            stdout, stderr = follow_process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            follow_process.terminate()
            stdout, stderr = follow_process.communicate()
        follow_result = RunApxResult(
            code=follow_process.returncode or 0,
            out=stdout,
            err=stderr,
        )

        assert follow_result.code == 0

        print("\n full follow result in out: \n")
        for line in follow_result.out.split("\n"):
            print(f" - {line}")

        assert "dev::process" in follow_result.out
        assert "dev::server" in follow_result.out
        assert "shutdown complete" in follow_result.out

    finally:
        print("Stopping dev server as a cleanup step")
        stop_result = run_apx_subprocess(["dev", "stop"], e2e_init)
        print(
            f"cleanup stop result: {stop_result} with error: {stop_result.err} and output: {stop_result.out}"
        )


def test_dev_server_refreshes_openapi(e2e_init: Path) -> None:
    try:
        print(f"Starting dev server in {e2e_init}")
        start_result = run_apx_subprocess(["dev", "start"], e2e_init)
        assert start_result.code == 0

        # replace the /src/{app_slug}/backend/router.py with a modified version that returns a different version

        new_content = """
from typing import Annotated
from fastapi import APIRouter, Depends
from .models import VersionOut
from databricks.sdk import WorkspaceClient
from databricks.sdk.service.iam import User as UserOut
from .dependencies import get_obo_ws
from .config import conf

api = APIRouter(prefix=conf.api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()
        """

        (e2e_init / "src" / "test_app" / "backend" / "router.py").write_text(
            new_content
        )

        # wait for the watcher to detect changes and regenerate (poll=500ms + debounce=300ms + buffer)
        time.sleep(2)

        # check the server logs
        print(f"Checking server logs in {e2e_init}")
        logs_result = run_apx_subprocess(["dev", "logs"], e2e_init)
        print(
            f"logs result: {logs_result} with error: {logs_result.err} and output: {logs_result.out}"
        )

        # should contain:
        assert "regenerating OpenAPI" in logs_result.out

        # check that "currentUser" is not in the generated api.ts
        api_ts_path = e2e_init / "src" / "test_app" / "ui" / "lib" / "api.ts"
        deadline = time.time() + 5
        while time.time() < deadline and not api_ts_path.exists():
            time.sleep(0.5)
        assert api_ts_path.exists(), (
            f"api.ts file not found at {api_ts_path} after {time.time() - deadline} seconds"
        )
        api_ts_content = api_ts_path.read_text()
        assert "currentUser" not in api_ts_content

    finally:
        print("Stopping dev server as a cleanup step")
        stop_result = run_apx_subprocess(["dev", "stop"], e2e_init)
        print(
            f"cleanup stop result: {stop_result} with error: {stop_result.err} and output: {stop_result.out}"
        )
