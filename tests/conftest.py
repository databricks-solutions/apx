import asyncio
import os
from contextlib import asynccontextmanager
from dataclasses import dataclass
from importlib import resources
from pathlib import Path
from typing import AsyncIterator

import pytest
from apx._core import run_cli


# Session-scoped temporary cache directory for parallel test safety
@pytest.fixture(scope="session", autouse=True)
def apx_cache_dir(tmp_path_factory: pytest.TempPathFactory):
    """Create a temporary cache directory for the test session.

    This sets APX_CACHE_DIR environment variable to isolate tests from
    the user's real cache and avoid parallel test conflicts.
    """
    cache_dir = tmp_path_factory.mktemp("apx_cache")
    os.environ["APX_CACHE_DIR"] = str(cache_dir)
    yield cache_dir
    # Clean up environment variable after session
    os.environ.pop("APX_CACHE_DIR", None)


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
        # Kill process - don't wait() as it deadlocks with PIPE due to pipe buffering
        # The OS will clean up the zombie process
        print(f"[cleanup] Starting cleanup, returncode={process.returncode}")
        if process.returncode is None:
            print("[cleanup] Killing process...")
            process.kill()
            # Don't call wait() - it deadlocks when stdout/stderr use PIPE
            # See: https://github.com/python/cpython/issues/119710

        # Close the transport to avoid "Event loop is closed" warning in __del__
        try:
            transport = getattr(process, "_transport", None)
            if transport is not None:
                transport.close()
        except Exception:
            pass  # Ignore any errors during transport cleanup
        print("[cleanup] Cleanup complete")


def _build_init_args(
    project_path: Path,
    *,
    name: str = "test-app",
    template: str = "essential",
    layout: str = "basic",
    assistant: str = "cursor",
    profile: str = "DEFAULT",
    skip_build: bool = True,
    skip_dependencies: bool = False,
) -> list[str]:
    """Build CLI arguments for init command.

    Args:
        project_path: Path where the project will be created
        name: Project name
        template: Template to use (essential/stateful)
        layout: Layout to use (basic/sidebar)
        assistant: Assistant to configure (cursor/vscode/codex/claude)
        profile: Databricks profile name
        skip_build: Whether to skip the build step
        skip_dependencies: Whether to skip both backend and frontend dependencies

    Returns:
        List of CLI arguments (without 'apx' prefix for async, with for sync)
    """
    args = [
        "init",
        str(project_path),
        "--assistant",
        assistant,
        "--layout",
        layout,
        "--template",
        template,
        "--profile",
        profile,
        "--name",
        name,
    ]
    if skip_build:
        args.append("--skip-build")
    if skip_dependencies:
        args.extend(["--skip-backend-dependencies", "--skip-frontend-dependencies"])
    return args


def _init_project(
    project_path: Path,
    *,
    name: str = "test-app",
    template: str = "minimal",
    layout: str = "basic",
    assistant: str = "cursor",
    profile: str = "DEFAULT",
    skip_build: bool = True,
) -> None:
    """Initialize an apx project with editable apx installation (synchronous).

    Args:
        project_path: Path where the project will be created
        name: Project name
        template: Template to use (essential/stateful)
        layout: Layout to use (basic/sidebar)
        assistant: Assistant to configure (cursor/vscode/codex/claude)
        profile: Databricks profile name
        skip_build: Whether to skip the build step
    """
    # Set APX_DEV_PATH environment variable for editable installation
    os.environ["APX_DEV_PATH"] = str(Path(apx_source_dir).resolve().absolute())

    try:
        args = ["apx"] + _build_init_args(
            project_path,
            name=name,
            template=template,
            layout=layout,
            assistant=assistant,
            profile=profile,
            skip_build=skip_build,
        )

        exit_code = run_cli(args)
        assert exit_code == 0, f"Failed to initialize project at {project_path}"
    finally:
        # Clean up environment variable after initialization
        os.environ.pop("APX_DEV_PATH", None)


async def init_project_async(
    project_path: Path,
    *,
    name: str = "test-app",
    template: str = "essential",
    layout: str = "basic",
    assistant: str = "cursor",
    profile: str = "DEFAULT",
    skip_build: bool = True,
    skip_dependencies: bool = True,
) -> CliResult:
    """Initialize an apx project with editable apx installation (async).

    This is the async version that uses run_cli_async. Editable apx installation
    is controlled via the APX_DEV_PATH environment variable.

    Args:
        project_path: Path where the project will be created
        name: Project name
        template: Template to use (essential/stateful)
        layout: Layout to use (basic/sidebar)
        assistant: Assistant to configure (cursor/vscode/codex/claude)
        profile: Databricks profile name
        skip_build: Whether to skip the build step
        skip_dependencies: Whether to skip both backend and frontend dependencies

    Returns:
        CliResult with returncode, stdout, stderr
    """
    args = _build_init_args(
        project_path,
        name=name,
        template=template,
        layout=layout,
        assistant=assistant,
        profile=profile,
        skip_build=skip_build,
        skip_dependencies=skip_dependencies,
    )

    # Set APX_DEV_PATH for editable installation
    env = {"APX_DEV_PATH": str(Path(apx_source_dir).resolve().absolute())}

    result = await run_cli_async(args, cwd=project_path, env=env)
    return result


@pytest.fixture(scope="session")
def common_project(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """Session-scoped project with both backend and frontend deps installed.

    This fixture is created ONCE per test session and shared across all tests.
    Use `isolated_project` if you need a fresh project per test with full dependencies.

    WARNING: Tests using this fixture should NOT modify the project state,
    as changes will affect other tests.
    """
    import sys

    project_path = tmp_path_factory.mktemp("common_project")
    _init_project(project_path)
    sys.path.insert(0, str(project_path / "src"))
    return project_path


@pytest.fixture
def isolated_project(tmp_path: Path) -> Path:
    """Function-scoped project with full initialization.

    This fixture creates a fresh project for each test with all dependencies
    installed. Use this when tests need to modify the project or need isolation.

    Note: This is slower than common_project as it runs full init each time.
    """
    project_path = tmp_path / "project"
    project_path.mkdir(parents=True, exist_ok=True)
    _init_project(project_path)
    return project_path
