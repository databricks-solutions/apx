"""Shared test fixtures for the APX test suite.

Builds the APX wheel for Linux via Docker cross-compilation and creates a
Docker image usable by both ``tests/integration/`` and ``tests/telemetry/``.
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

import pytest

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = PROJECT_ROOT / "scripts" / "bench"
APP_SRC = BENCH_DIR / "app"
REQS_SRC = BENCH_DIR / "databricks" / "apps" / "bench-apx" / "requirements.txt"
DOCKERFILE_SRC = PROJECT_ROOT / "docker" / "Dockerfile.apx-local"
CARGO_TOML = PROJECT_ROOT / "crates" / "apx" / "Cargo.toml"

CROSS_IMAGE = "apx-cross-bench:latest"
CROSS_TARGET = "x86_64-unknown-linux-gnu"
TEST_IMAGE = "apx-integration-test:latest"


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
# Build helpers
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


def _docker_build(build_dir: Path, tag: str) -> None:
    """Build a Docker image using BuildKit, mounting ~/.pip/pip.conf if present."""
    cmd = [
        "docker",
        "build",
        "--platform",
        "linux/amd64",
        "--rm",
        "-t",
        tag,
    ]

    pip_conf = Path.home() / ".pip" / "pip.conf"
    if pip_conf.is_file():
        cmd += ["--secret", f"id=pip_conf,src={pip_conf}"]
        print(f"[build] Mounting {pip_conf} as BuildKit secret")

    cmd.append(str(build_dir))

    result = subprocess.run(
        cmd,
        cwd=str(PROJECT_ROOT),
        env={**__import__("os").environ, "DOCKER_BUILDKIT": "1"},
        check=False,
    )
    if result.returncode != 0:
        pytest.fail("Docker image build failed (see output above)")


# ---------------------------------------------------------------------------
# Shared image build fixture
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def apx_image(request: pytest.FixtureRequest) -> str:
    """Build the APX Docker image once per session. Returns the image tag."""
    skip_build = request.config.getoption("--skip-build")

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
        _docker_build(build_dir, TEST_IMAGE)
        print(f"[build] Image built in {time.monotonic() - t2:.1f}s")
    else:
        print("\n[build] --skip-build: reusing existing image")

    return TEST_IMAGE
