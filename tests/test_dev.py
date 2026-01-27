import asyncio
import json
from pathlib import Path

import httpx
from bs4 import BeautifulSoup
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type,
)

from conftest import run_cli_async, run_cli_background


async def test_dev_server_start_stop_with_logs(isolated_project: Path) -> None:
    try:
        print(f"Starting dev server in {isolated_project}")
        start_result = await run_cli_async(["dev", "start"], cwd=isolated_project)
        if start_result.returncode != 0:
            for line in start_result.stderr.split("\n"):
                print(f"stderr: {line}")
            for line in start_result.stdout.split("\n"):
                print(f"stdout: {line}")
            raise RuntimeError(f"Failed to start dev server: {start_result.stderr}")

        # check the server logs
        print(f"Checking server logs in {isolated_project}")
        logs_result = await run_cli_async(["dev", "logs"], cwd=isolated_project)
        print(
            f"logs result: returncode={logs_result.returncode} with error: {logs_result.stderr} and output: {logs_result.stdout}"
        )

        # Start logs follow in background with debug logging
        env = {"APX_LOG": "debug", "APX_COLLECT_LOGS": "1"}
        async with run_cli_background(
            ["dev", "logs", "--follow", str(isolated_project)],
            cwd=isolated_project,
            env=env,
        ) as follow_process:
            await asyncio.sleep(0.2)
            stop_result = await run_cli_async(["dev", "stop"], cwd=isolated_project)
            assert stop_result.returncode == 0

            # Wait for process to finish and collect output
            try:
                stdout_bytes, _ = await asyncio.wait_for(
                    follow_process.communicate(), timeout=10
                )
                stdout = stdout_bytes.decode("utf-8") if stdout_bytes else ""
            except asyncio.TimeoutError:
                follow_process.terminate()
                stdout_bytes, _ = await follow_process.communicate()
                stdout = stdout_bytes.decode("utf-8") if stdout_bytes else ""

            assert follow_process.returncode == 0

            print("\n full follow result in out: \n")
            for line in stdout.split("\n"):
                print(f" - {line}")

            assert "Dev server stopped" in stdout

    finally:
        print("Stopping dev server as a cleanup step")
        stop_result = await run_cli_async(["dev", "stop"], cwd=isolated_project)
        print(
            f"cleanup stop result: returncode={stop_result.returncode} with error: {stop_result.stderr} and output: {stop_result.stdout}"
        )


async def test_dev_server_refreshes_openapi(isolated_project: Path) -> None:
    try:
        print(f"Starting dev server in {isolated_project}")
        start_result = await run_cli_async(["dev", "start"], cwd=isolated_project)
        assert start_result.returncode == 0
        await asyncio.sleep(4)

        # replace the /src/{app_slug}/backend/router.py with a modified version that returns a different version

        new_content = """
from typing import Annotated
from fastapi import APIRouter, Depends
from .models import VersionOut
from databricks.sdk import WorkspaceClient
from databricks.sdk.service.iam import User as UserOut
from .dependencies import get_obo_ws
from .._metadata import api_prefix

api = APIRouter(prefix=api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()
        """

        (isolated_project / "src" / "test_app" / "backend" / "router.py").write_text(
            new_content
        )

        await asyncio.sleep(2)

        # check the server logs
        print(f"Checking server logs in {isolated_project}")
        logs_result = await run_cli_async(["dev", "logs"], cwd=isolated_project)
        print(
            f"logs result: returncode={logs_result.returncode} with error: {logs_result.stderr} and output: {logs_result.stdout}"
        )

        # should contain distinct messages for initial generation and regeneration
        assert "Initial OpenAPI generated" in logs_result.stdout, (
            "Initial generation should complete"
        )
        assert "Python change detected, regenerating OpenAPI" in logs_result.stdout, (
            "File change should trigger regeneration"
        )

        # check that "currentUser" is not in the generated api.ts
        api_ts_path = isolated_project / "src" / "test_app" / "ui" / "lib" / "api.ts"
        deadline = asyncio.get_event_loop().time() + 5
        while asyncio.get_event_loop().time() < deadline and not api_ts_path.exists():
            await asyncio.sleep(0.5)
        assert api_ts_path.exists(), f"api.ts file not found at {api_ts_path}"
        api_ts_content = api_ts_path.read_text()
        assert "currentUser" not in api_ts_content
    finally:
        print("Stopping dev server as a cleanup step")
        stop_result = await run_cli_async(["dev", "stop"], cwd=isolated_project)
        print(
            f"cleanup stop result: returncode={stop_result.returncode} with error: {stop_result.stderr} and output: {stop_result.stdout}"
        )


async def test_dev_server_proxies(isolated_project: Path) -> None:
    test_failed = False
    failure_exception: BaseException | None = None

    try:
        print(f"Starting dev server in {isolated_project}")
        start_result = await run_cli_async(["dev", "start"], cwd=isolated_project)
        assert start_result.returncode == 0

        # read .apx/dev.lock to get the backend and frontend ports
        dev_lock_path = isolated_project / ".apx" / "dev.lock"
        dev_lock_content = dev_lock_path.read_text()
        dev_lock_json = json.loads(dev_lock_content)
        dev_port = dev_lock_json.get("port")
        assert dev_port is not None

        print(
            f"Dev server started at http://localhost:{dev_port}, using it to test proxies"
        )

        # let the frontend/backend processes start
        await asyncio.sleep(2)

        http_client = httpx.AsyncClient()

        with_retry = retry(
            stop=stop_after_attempt(5),
            wait=wait_exponential(multiplier=1, min=1, max=10),
            retry=retry_if_exception_type((httpx.RequestError, httpx.HTTPStatusError)),
        )

        @with_retry
        async def fetch_backend() -> httpx.Response:
            resp = await http_client.get(f"http://localhost:{dev_port}/api/version")
            resp.raise_for_status()
            return resp

        backend_response = await fetch_backend()
        assert backend_response.status_code == 200
        assert backend_response.json().get("version") is not None

        @with_retry
        async def fetch_frontend() -> httpx.Response:
            resp = await http_client.get(f"http://localhost:{dev_port}/")
            resp.raise_for_status()
            return resp

        frontend_response = await fetch_frontend()
        assert frontend_response.status_code == 200
        # verify response is a valid HTML page
        soup = BeautifulSoup(frontend_response.text, "html.parser")
        assert soup.find("title") is not None
        assert soup.find("html") is not None

    except Exception as e:
        test_failed = True
        failure_exception = e

    finally:
        if test_failed:
            print("\n" + "=" * 60)
            print("TEST FAILED - Collecting debug logs")
            print("=" * 60)

            # Collect dev server logs
            print("\n--- Dev Server Logs ---")
            logs_result = await run_cli_async(["dev", "logs"], cwd=isolated_project)
            print(f"logs returncode: {logs_result.returncode}")
            if logs_result.stdout:
                print("stdout:")
                for line in logs_result.stdout.split("\n"):
                    print(f"  {line}")
            if logs_result.stderr:
                print("stderr:")
                for line in logs_result.stderr.split("\n"):
                    print(f"  {line}")

            # Check dev.lock contents
            dev_lock_path = isolated_project / ".apx" / "dev.lock"
            if dev_lock_path.exists():
                print("\n--- dev.lock contents ---")
                print(dev_lock_path.read_text())

            # Check status
            print("\n--- Dev Server Status ---")
            status_result = await run_cli_async(["dev", "status"], cwd=isolated_project)
            print(f"status returncode: {status_result.returncode}")
            if status_result.stdout:
                print(f"stdout: {status_result.stdout}")
            if status_result.stderr:
                print(f"stderr: {status_result.stderr}")

            print("=" * 60 + "\n")

        print("Stopping dev server as a cleanup step")
        stop_result = await run_cli_async(["dev", "stop"], cwd=isolated_project)
        print(
            f"cleanup stop result: returncode={stop_result.returncode} with error: {stop_result.stderr} and output: {stop_result.stdout}"
        )

        if test_failed and failure_exception:
            raise failure_exception
