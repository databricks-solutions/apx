"""APX integration test fixtures.

Builds the APX wheel for Linux via Docker cross-compilation, creates a Docker
image, and manages containers for the test session. Docker logs are collected
and printed on test failure.
"""

from __future__ import annotations

import base64
import csv
import datetime
import hashlib
import io
import re
import shutil
import subprocess
import time
import zipfile
from pathlib import Path
from typing import Generator, Literal

import docker
import docker.errors
import docker.models.containers
import httpx
import pytest

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DIR = PROJECT_ROOT / "scripts" / "bench"
APP_SRC = BENCH_DIR / "app"
REQS_SRC = BENCH_DIR / "databricks" / "apps" / "bench-apx" / "requirements.txt"
DOCKERFILE_SRC = PROJECT_ROOT / "docker" / "Dockerfile.apx-local"
CARGO_TOML = PROJECT_ROOT / "crates" / "apx" / "Cargo.toml"

CROSS_IMAGE = "apx-cross-bench:latest"
CROSS_TARGET = "x86_64-unknown-linux-gnu"
TEST_IMAGE = "apx-integration-test:latest"
CONTAINER_NAME = "apx-integration-test"

# Module-level state for the pytest hook to access.
_container: docker.models.containers.Container | None = None
_any_test_failed = False


# ---------------------------------------------------------------------------
# pytest CLI option
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--skip-build",
        action="store_true",
        default=False,
        help="Skip APX cross-compilation and Docker image build; reuse existing image.",
    )


# ---------------------------------------------------------------------------
# Docker log collection on failure
# ---------------------------------------------------------------------------


@pytest.hookimpl(tryfirst=True, hookwrapper=True)
def pytest_runtest_makereport(
    item: pytest.Item, call: pytest.CallInfo[None]
):  # noqa: ARG001
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
# Build helpers (extracted from scripts/bench/main.py)
# ---------------------------------------------------------------------------


def _stub_agent_binary() -> None:
    """Create a zero-byte agent stub so the Rust build.rs doesn't fail."""
    stub = PROJECT_ROOT / ".bins" / "agent" / "apx-agent-linux-x64"
    stub.parent.mkdir(parents=True, exist_ok=True)
    stub.touch()
    print(f"[build] Stubbed agent binary: {stub}")


def _stamp_wheel(wheel_path: Path, new_version: str) -> Path:
    """Repack a wheel with *new_version* in filename + metadata."""
    tmp_dir = wheel_path.parent / "_repack"
    if tmp_dir.exists():
        shutil.rmtree(tmp_dir)

    with zipfile.ZipFile(wheel_path, "r") as zf:
        zf.extractall(tmp_dir)

    dist_infos = list(tmp_dir.glob("*.dist-info"))
    assert len(dist_infos) == 1, f"Expected 1 dist-info, found {len(dist_infos)}"
    old_di = dist_infos[0]

    meta_path = old_di / "METADATA"
    meta_text = meta_path.read_text()
    meta_text = re.sub(r"(?m)^Version: .+$", f"Version: {new_version}", meta_text)
    meta_path.write_text(meta_text)

    old_name = old_di.name
    new_di_name = re.sub(
        r"-[\d][^-]*\.dist-info$", f"-{new_version}.dist-info", old_name
    )
    new_di = old_di.rename(old_di.parent / new_di_name)

    record_path = new_di / "RECORD"
    record_rows: list[list[str]] = []
    for fpath in sorted(tmp_dir.rglob("*")):
        if fpath.is_dir():
            continue
        rel = fpath.relative_to(tmp_dir).as_posix()
        if rel == f"{new_di_name}/RECORD":
            record_rows.append([rel, "", ""])
            continue
        data = fpath.read_bytes()
        digest = (
            base64.urlsafe_b64encode(hashlib.sha256(data).digest())
            .rstrip(b"=")
            .decode()
        )
        record_rows.append([rel, f"sha256={digest}", str(len(data))])

    buf = io.StringIO()
    csv.writer(buf).writerows(record_rows)
    record_path.write_text(buf.getvalue())

    old_stem = wheel_path.stem
    new_stem = re.sub(r"^(apx)-[\d][^-]*", rf"\1-{new_version}", old_stem)
    new_wheel = wheel_path.parent / f"{new_stem}.whl"

    with zipfile.ZipFile(new_wheel, "w", zipfile.ZIP_DEFLATED) as zf:
        for fpath in sorted(tmp_dir.rglob("*")):
            if fpath.is_dir():
                continue
            zf.write(fpath, fpath.relative_to(tmp_dir).as_posix())

    shutil.rmtree(tmp_dir)
    if wheel_path != new_wheel:
        wheel_path.unlink()

    return new_wheel


def _build_apx_wheel(dest_dir: Path) -> Path:
    """Cross-compile the APX wheel for linux/amd64 via Docker + maturin."""
    _stub_agent_binary()

    dest_dir.mkdir(parents=True, exist_ok=True)
    for old in dest_dir.glob("apx-*.whl"):
        old.unlink()

    sccache_dir = Path.home() / "Library" / "Caches" / "Mozilla.sccache"
    sccache_dir.mkdir(parents=True, exist_ok=True)
    cargo_home = Path.home() / ".cargo"

    print(f"Starting maturin build with sccache dir: {sccache_dir}")

    result = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "-v",
            f"{PROJECT_ROOT}:/io",
            "-v",
            f"{sccache_dir}:/cache/sccache",
            "-v",
            f"{cargo_home / 'registry'}:/root/.cargo/registry",
            "-v",
            f"{cargo_home / 'git'}:/root/.cargo/git",
            CROSS_IMAGE,
            "maturin",
            "build",
            "--release",
            "--target",
            CROSS_TARGET,
            "-i",
            "python3.11",
            "--out",
            str(Path("/io") / dest_dir.relative_to(PROJECT_ROOT)),
            "--manifest-path",
            "crates/apx/Cargo.toml",
        ],
        cwd=str(PROJECT_ROOT),
        check=False,
    )
    if result.returncode != 0:
        pytest.fail("maturin cross-compilation failed (see output above)")

    wheels = sorted(
        dest_dir.glob("apx-*.whl"), key=lambda p: p.stat().st_mtime, reverse=True
    )
    assert wheels, "No APX wheel found after maturin build"

    m = re.search(r'^version\s*=\s*"([^"]+)"', CARGO_TOML.read_text(), re.MULTILINE)
    assert m, "Could not find version in crates/apx/Cargo.toml"
    base_version = m.group(1)
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d%H%M%S")
    build_version = f"{base_version}+test.{ts}"

    stamped = _stamp_wheel(wheels[0], build_version)
    print(f"[build] Wheel: {stamped.name}  (version {build_version})")
    return stamped


def _assemble_bench_app(wheel_path: Path, build_dir: Path) -> Path:
    """Assemble the bench-apx app directory for Docker build context."""
    if build_dir.exists():
        shutil.rmtree(build_dir)
    build_dir.mkdir(parents=True)

    shutil.copytree(APP_SRC, build_dir / "app")
    dest_reqs = build_dir / "requirements.txt"
    shutil.copy2(REQS_SRC, dest_reqs)
    shutil.copy2(wheel_path, build_dir / wheel_path.name)

    with open(dest_reqs, "a") as f:
        f.write(f"./{wheel_path.name}\n")

    shutil.copy2(DOCKERFILE_SRC, build_dir / "Dockerfile")

    print(f"[build] Assembled app dir: {build_dir}")
    return build_dir


# ---------------------------------------------------------------------------
# Shared image build fixture
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def apx_image(request: pytest.FixtureRequest) -> str:
    """Build the APX Docker image once per session. Returns the image tag."""
    skip_build = request.config.getoption("--skip-build")
    dk = docker.from_env()

    if not skip_build:
        t0 = time.monotonic()
        print("\n[build] Cross-compiling APX wheel for linux/amd64...")
        wheel_dest = PROJECT_ROOT / "tests" / "integration" / ".build" / ".wheels"
        wheel_path = _build_apx_wheel(wheel_dest)
        print(f"[build] Wheel built in {time.monotonic() - t0:.1f}s")

        t1 = time.monotonic()
        print("[build] Assembling app directory...")
        build_dir = PROJECT_ROOT / "tests" / "integration" / ".build" / "bench-apx"
        _assemble_bench_app(wheel_path, build_dir)
        print(f"[build] Assembly done in {time.monotonic() - t1:.1f}s")

        t2 = time.monotonic()
        print(f"[build] Building Docker image {TEST_IMAGE}...")
        dk.images.build(
            path=str(build_dir),
            dockerfile="Dockerfile",
            tag=TEST_IMAGE,
            platform="linux/amd64",
            rm=True,
        )
        print(f"[build] Image built in {time.monotonic() - t2:.1f}s")
    else:
        print("\n[build] --skip-build: reusing existing image")

    return TEST_IMAGE


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

    # ── Remove stale container ──
    try:
        stale = dk.containers.get(CONTAINER_NAME)
        stale.remove(force=True)
    except docker.errors.NotFound:
        pass

    # ── Start container ──
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

    # ── Resolve mapped port ──
    container.reload()
    host_port = container.ports["8000/tcp"][0]["HostPort"]
    base_url = f"http://localhost:{host_port}"
    print(f"[container] Mapped port: {host_port}")

    # ── Wait for readiness ──
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

    # ── Teardown ──
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
