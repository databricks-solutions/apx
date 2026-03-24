"""APX integration test fixtures.

Manages containers for the test session. Docker logs are collected and
printed on test failure. The ``apx_image`` session fixture is defined in
the parent ``tests/conftest.py`` and shared across test directories.
"""

from __future__ import annotations

import time
from typing import Generator, Literal

import docker
import docker.errors
import docker.models.containers
import httpx
import pytest

CONTAINER_NAME = "apx-integration-test"

_container: docker.models.containers.Container | None = None
_any_test_failed = False


# ---------------------------------------------------------------------------
# Docker log collection on failure
# ---------------------------------------------------------------------------


@pytest.hookimpl(tryfirst=True, hookwrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo[None]):  # noqa: ARG001
    outcome = yield
    report = outcome.get_result()
    if report.when == "call" and report.failed:
        global _any_test_failed
        _any_test_failed = True
        _print_container_logs(
            tail=80, header=f"Container logs (last 80 lines) after FAILED {item.nodeid}"
        )


def _print_container_logs(
    *, tail: int | Literal["all"] = "all", header: str = "Container logs"
) -> None:
    if _container is None:
        return
    try:
        logs = _container.logs(tail=tail).decode("utf-8", errors="replace")
    except Exception:
        return
    separator = "=" * 72
    print(f"\n{separator}")
    print(f"  {header}")
    print(separator)
    print(logs)
    print(separator)


# ---------------------------------------------------------------------------
# Container helpers
# ---------------------------------------------------------------------------


def wait_for_healthy(base_url: str, *, timeout: float = 10) -> None:
    """Poll the health endpoint until the container is ready."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            r = httpx.get(f"{base_url}/api/health", timeout=2.0)
            if r.status_code == 200:
                return
        except (httpx.ConnectError, httpx.ReadError, httpx.TimeoutException):
            pass
        time.sleep(1.0)


def print_container_logs(
    container: docker.models.containers.Container,
    *,
    tail: int | Literal["all"] = "all",
    header: str = "Container logs",
) -> None:
    """Print Docker container logs."""
    try:
        logs = container.logs(tail=tail).decode("utf-8", errors="replace")
    except Exception:
        return
    separator = "=" * 72
    print(f"\n{separator}")
    print(f"  {header}")
    print(separator)
    print(logs)
    print(separator)


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def apx_container(apx_image: str) -> Generator[str]:
    """Start the APX container, yield base URL."""
    global _container
    dk = docker.from_env()

    try:
        stale = dk.containers.get(CONTAINER_NAME)
        stale.remove(force=True)
    except docker.errors.NotFound:
        pass

    print("[container] Starting APX container...")
    t0 = time.monotonic()
    container = dk.containers.run(
        apx_image,
        command=["apx", "serve", "app.main", "--host", "0.0.0.0", "--workers", "2"],
        name=CONTAINER_NAME,
        platform="linux/amd64",
        ports={"8000/tcp": None},
        environment={
            "APX_BENCH_PROFILE": "1",
            "APX_PERF": "1",
            "APX_LOG": "trace",
        },
        detach=True,
    )
    _container = container

    container.reload()
    host_port = container.ports["8000/tcp"][0]["HostPort"]
    base_url = f"http://localhost:{host_port}"
    print(f"[container] Mapped port: {host_port}")

    print("[container] Waiting for health check...")
    try:
        wait_for_healthy(base_url)
    except Exception:
        _print_container_logs(header="Container logs (startup failed)")
        container.stop(timeout=5)
        container.remove()
        _container = None
        pytest.fail("Container did not become healthy")

    elapsed = time.monotonic() - t0
    print(f"[container] Ready in {elapsed:.1f}s at {base_url}")

    yield base_url

    if _any_test_failed:
        _print_container_logs(header="Full container logs (session had failures)")
    print("\n")
    print("[container] Stopping and removing container...")
    container.stop(timeout=10)
    container.remove()
    _container = None


@pytest.fixture(scope="session")
def container(apx_container: str) -> docker.models.containers.Container:  # noqa: ARG001
    """Expose the running Docker container for log inspection."""
    assert _container is not None, "container not started"
    return _container


@pytest.fixture(scope="session")
def client(apx_container: str) -> Generator[httpx.Client]:
    """Session-scoped httpx client pointed at the APX container."""
    with httpx.Client(base_url=apx_container, timeout=30.0) as c:
        yield c
