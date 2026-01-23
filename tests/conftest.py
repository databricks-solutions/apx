import asyncio
import os
import shutil
from contextlib import asynccontextmanager
from dataclasses import dataclass
from importlib import resources
from pathlib import Path
from typing import AsyncIterator

import pytest
from apx._core import run_cli


@dataclass
class CliResult:
    returncode: int
    stdout: str
    stderr: str


apx_source_dir: str = str(Path(str(resources.files("apx"))).parent.parent)


async def run_cli_async(
    args: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
) -> CliResult:
    """Run CLI command and wait for completion.

    Args:
        args: CLI arguments (e.g., ["dev", "start"])
        cwd: Working directory for the command
        env: Environment variables (merged with current environment)
        timeout: Timeout in seconds

    Returns:
        CliResult with returncode, stdout, stderr
    """
    full_env = os.environ.copy()
    if env:
        full_env.update(env)
    if "APX_LOG" not in full_env:
        full_env["APX_LOG"] = "debug"

    process = await asyncio.create_subprocess_exec(
        "uv",
        "run",
        "apx",
        *args,
        cwd=str(cwd) if cwd else None,
        env=full_env,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )

    try:
        stdout_bytes, stderr_bytes = await asyncio.wait_for(
            process.communicate(), timeout=timeout
        )
        stdout = stdout_bytes.decode("utf-8")
        stderr = stderr_bytes.decode("utf-8")
        returncode = process.returncode or 0
    except asyncio.TimeoutError:
        process.kill()
        await process.wait()
        raise TimeoutError(
            f"Command timed out after {timeout}s: uv run apx {' '.join(args)}"
        )

    return CliResult(returncode=returncode, stdout=stdout, stderr=stderr)


@asynccontextmanager
async def run_cli_background(
    args: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> AsyncIterator[asyncio.subprocess.Process]:
    """Run CLI command in background with process cleanup.

    Args:
        args: CLI arguments (e.g., ["dev", "logs", "--follow"])
        cwd: Working directory for the command
        env: Environment variables (merged with current environment)

    Yields:
        Process object for the running command

    Example:
        async with run_cli_background(["dev", "logs", "--follow"], cwd=project) as proc:
            # Process is running
            await asyncio.sleep(1)
            # Process is terminated on exit
    """
    full_env = os.environ.copy()
    if env:
        full_env.update(env)

    process = await asyncio.create_subprocess_exec(
        "uv",
        "run",
        "apx",
        *args,
        cwd=str(cwd) if cwd else None,
        env=full_env,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )

    try:
        yield process
    finally:
        # Ensure process is terminated
        if process.returncode is None:
            process.terminate()
            try:
                await asyncio.wait_for(process.wait(), timeout=5.0)
            except asyncio.TimeoutError:
                process.kill()
                await process.wait()


@pytest.fixture(scope="session")
def common_project(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """Session-scoped project with both backend and frontend deps installed.

    This fixture is created ONCE per test session and shared across all tests.
    Use `isolated_project` if you need a fresh copy per test.
    """
    import sys

    project_path = tmp_path_factory.mktemp("common_project")

    # Set APX_DEV_PATH environment variable for editable installation
    os.environ["APX_DEV_PATH"] = str(Path(apx_source_dir).resolve().absolute())

    try:
        exit_code = run_cli(
            [
                "apx",
                "init",
                str(project_path),
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
        assert exit_code == 0, "Failed to initialize common_project"
        sys.path.insert(0, str(project_path / "src"))
        return project_path
    finally:
        # Clean up environment variable after initialization
        os.environ.pop("APX_DEV_PATH", None)


@pytest.fixture
def isolated_project(common_project: Path, tmp_path: Path) -> Path:
    """Function-scoped project copied from common_project.

    This fixture provides a fresh copy of the common_project for each test,
    ensuring test isolation while avoiding the cost of re-running init + deps.
    """
    project_path = tmp_path / "project"
    shutil.copytree(common_project, project_path, dirs_exist_ok=True)
    return project_path
