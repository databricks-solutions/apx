# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13", "httpx>=0.27", "pydantic>=2", "typer>=0.15", "databricks-sdk>=0.74"]
# ///
"""APX app build and deployment helpers.

Handles cross-compilation of the APX wheel, assembly of Databricks app
directories, workspace-FS upload for bench-apx, and app lifecycle management
via the Databricks SDK (bypassing DABs, which lack telemetry support).
"""

from __future__ import annotations

import datetime
import shutil
import subprocess
import time
from pathlib import Path

import typer
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn

from databricks.sdk import WorkspaceClient
from databricks.sdk.errors import BadRequest
from databricks.sdk.service.apps import (
    App,
    AppDeployment,
    AppDeploymentMode,
    AppDeploymentState,
    ApplicationState,
    ComputeState,
    EnvVar,
)
from databricks.sdk.service.workspace import ImportFormat

console = Console()

# ---------------------------------------------------------------------------
# Paths & constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DIR = Path(__file__).resolve().parent
DATABRICKS_DIR = BENCH_DIR / "databricks"

CROSS_TARGET = "x86_64-unknown-linux-gnu"

DOCKER_IMAGE = "apx-cross-bench:latest"
LOCAL_IMAGE = "apx-local:latest"
LOCAL_CONTAINER = "apx-local-bench"
LOCAL_DOCKERFILE = PROJECT_ROOT / "docker" / "Dockerfile.apx-local"

APX_APP_NAME = "bench-apx"

# Command + env vars for the bench-apx app deployment.
APX_DEPLOY_COMMAND = [
    "apx",
    "serve",
    "app.main",
    "--host",
    "0.0.0.0",
    "--workers",
    "2",
]
APX_DEPLOY_ENV_VARS = [
    EnvVar(name="APX_BENCH_SERVER", value="apx"),
    EnvVar(name="APX_BENCH_PROFILE", value="1"),
    EnvVar(name="APX_PERF", value="1"),
]

# ---------------------------------------------------------------------------
# Wheel stamping
# ---------------------------------------------------------------------------


def _stamp_wheel(wheel_path: Path, new_version: str) -> Path:
    """Repack a wheel with *new_version* baked into filename + metadata.

    Wheels are zip archives.  We rewrite three things:
    1. The ``Version:`` header inside ``METADATA``
    2. The ``RECORD`` manifest (SHA-256 digests of every file)
    3. The outer filename itself

    Returns the path to the new wheel (old one is deleted).
    """
    import base64
    import csv
    import hashlib
    import io
    import re
    import zipfile

    tmp_dir = wheel_path.parent / "_repack"
    if tmp_dir.exists():
        shutil.rmtree(tmp_dir)

    # ── unzip ──
    with zipfile.ZipFile(wheel_path, "r") as zf:
        zf.extractall(tmp_dir)

    # ── locate dist-info ──
    dist_infos = list(tmp_dir.glob("*.dist-info"))
    assert len(dist_infos) == 1, f"Expected 1 dist-info, found {len(dist_infos)}"
    old_di = dist_infos[0]

    # ── patch METADATA ──
    meta_path = old_di / "METADATA"
    meta_text = meta_path.read_text()
    meta_text = re.sub(r"(?m)^Version: .+$", f"Version: {new_version}", meta_text)
    meta_path.write_text(meta_text)

    # ── rename dist-info dir ──
    old_name = old_di.name  # e.g. apx-0.3.8.dist-info
    new_di_name = re.sub(
        r"-[\d][^-]*\.dist-info$", f"-{new_version}.dist-info", old_name
    )
    new_di = old_di.rename(old_di.parent / new_di_name)

    # ── regenerate RECORD ──
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

    # ── repack into new .whl ──
    old_stem = wheel_path.stem  # apx-0.3.8-cp311-cp311-...
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


# ---------------------------------------------------------------------------
# Wheel build (maturin cross-compilation)
# ---------------------------------------------------------------------------


def build_apx_wheel(dest_dir: Path) -> Path:
    """Cross-compile APX wheel for linux/amd64 using Docker."""
    import re

    # Stub the agent binary — crates/core/build.rs copies it and resources.rs
    # embeds it via include_bytes!(). The bench wheel only uses `apx serve`,
    # never the agent, so a zero-byte stub is fine.
    agent_stub = PROJECT_ROOT / ".bins" / "agent" / "apx-agent-linux-x64"
    agent_stub.parent.mkdir(parents=True, exist_ok=True)
    agent_stub.touch()
    console.print(f"[dim]Stubbed agent binary:[/] {agent_stub}")

    console.print("\n[bold blue]Building APX wheel via maturin (Docker)...[/]")
    dest_dir.mkdir(parents=True, exist_ok=True)
    for old in dest_dir.glob("apx-*.whl"):
        old.unlink()

    sccache_dir = Path.home() / "Library" / "Caches" / "Mozilla.sccache"
    sccache_dir.mkdir(parents=True, exist_ok=True)
    cargo_home = Path.home() / ".cargo"

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
            DOCKER_IMAGE,
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
        console.print("[red]Error:[/] maturin build failed")
        raise typer.Exit(1)

    wheels = sorted(
        dest_dir.glob("apx-*.whl"), key=lambda p: p.stat().st_mtime, reverse=True
    )
    assert wheels, "No APX wheel found after maturin build"

    cargo_toml = PROJECT_ROOT / "crates" / "apx" / "Cargo.toml"
    m = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml.read_text(), re.MULTILINE)
    assert m, "Could not find version in crates/apx/Cargo.toml"
    base_version = m.group(1)
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d%H%M%S")
    build_version = f"{base_version}+bench.{ts}"

    stamped = _stamp_wheel(wheels[0], build_version)
    console.print(f"[green]Built wheel:[/] {stamped.name}  (version {build_version})")
    return stamped


# ---------------------------------------------------------------------------
# Databricks assembly
# ---------------------------------------------------------------------------

DATABRICKS_APPS = {
    "bench_uvicorn": "bench-uvicorn",
    "bench_granian": "bench-granian",
    "bench_apx": APX_APP_NAME,
    "bench_bencher": "bench-bencher",
}


def _write_app_yaml(dest_dir: Path) -> None:
    """Write app.yaml for the bench-apx Databricks App runtime.

    Generated from APX_DEPLOY_COMMAND and APX_DEPLOY_ENV_VARS so it stays in
    sync with what AppDeployment passes to the API.
    See: https://docs.databricks.com/aws/en/dev-tools/databricks-apps/app-runtime
    """

    def _scalar(v: str) -> str:
        # Quote values that YAML would misinterpret as non-string scalars.
        try:
            float(v)
            return f'"{v}"'
        except ValueError:
            pass
        if v.lower() in {"true", "false", "null", "yes", "no", "on", "off"}:
            return f'"{v}"'
        return v

    lines = ["command:"]
    for arg in APX_DEPLOY_COMMAND:
        lines.append(f"  - {_scalar(arg)}")
    lines.append("env:")
    for ev in APX_DEPLOY_ENV_VARS:
        lines.append(f"  - name: {_scalar(ev.name or '')}")
        lines.append(f"    value: {_scalar(ev.value or '')}")
    (dest_dir / "app.yaml").write_text("\n".join(lines) + "\n")


def assemble_databricks_apps(wheel_path: Path | None) -> None:
    """Copy shared app code + per-app configs → .build/{app-name}/."""
    build_dir = DATABRICKS_DIR / ".build"
    app_src = BENCH_DIR / "app"
    bencher_src = BENCH_DIR / "bencher"

    for resource_key, app_name in DATABRICKS_APPS.items():
        app_build = build_dir / app_name
        if app_build.exists():
            shutil.rmtree(app_build)
        app_build.mkdir(parents=True)

        if app_name == "bench-bencher":
            shutil.copytree(bencher_src, app_build / "bencher")
            shutil.copy2(
                BENCH_DIR / "profile_analysis.py", app_build / "profile_analysis.py"
            )
            shutil.copy2(BENCH_DIR / "scenarios.json", app_build / "scenarios.json")
        else:
            shutil.copytree(app_src, app_build / "app")

        src_reqs = DATABRICKS_DIR / "apps" / app_name / "requirements.txt"
        dest_reqs = app_build / "requirements.txt"
        shutil.copy2(src_reqs, dest_reqs)

        if app_name == APX_APP_NAME and wheel_path is not None:
            shutil.copy2(wheel_path, app_build / wheel_path.name)
            with open(dest_reqs, "a") as f:
                f.write(f"./{wheel_path.name}\n")

        if app_name == APX_APP_NAME:
            _write_app_yaml(app_build)

        console.print(f"[green]Assembled:[/] {app_build}")


# ---------------------------------------------------------------------------
# Workspace FS upload for bench-apx
# ---------------------------------------------------------------------------


def upload_apx_app(ws: WorkspaceClient) -> str:
    """Upload the assembled bench-apx directory to the workspace filesystem.

    Uploads .build/bench-apx/ to /Users/{username}/bench-apx via the workspace
    file API (no /Workspace prefix).  Returns the same path with the
    /Workspace prefix prepended — that is the form required by the Apps deploy
    API (source_code_path).

    Both sides therefore point at the same directory:
        workspace API : /Users/{username}/bench-apx
        deploy API    : /Workspace/Users/{username}/bench-apx
    """
    me = ws.current_user.me()
    username = me.user_name
    assert username, "Could not determine current Databricks user"

    upload_path = f"/Users/{username}/bench-apx"
    deploy_path = f"/Workspace{upload_path}"
    console.print(
        f"\n[bold blue]Uploading bench-apx to workspace:[/] {upload_path}"
        f"\n[dim]  deploy source_code_path will be: {deploy_path}[/]"
    )

    # Wipe the remote directory so stale files (old wheels, etc.) don't linger.
    try:
        ws.workspace.delete(upload_path, recursive=True)
        time.sleep(1)
    except Exception as exc:
        console.print(f"[dim]  directory did not exist, skipping wipe.[/]")
        console.print(f"[dim] Original error: {exc}[/]")
    ws.workspace.mkdirs(upload_path)

    local_build = DATABRICKS_DIR / ".build" / APX_APP_NAME
    if not local_build.exists():
        console.print(
            "[red]Error:[/] bench-apx build directory not found. Run 'build' first."
        )
        raise typer.Exit(1)

    _SKIP_SUFFIXES = {".pyc", ".pyo"}
    _SKIP_PARTS = {"__pycache__", ".DS_Store"}

    def _should_upload(p: Path) -> bool:
        if p.suffix in _SKIP_SUFFIXES:
            return False
        if _SKIP_PARTS.intersection(p.parts):
            return False
        return True

    files = sorted(
        f for f in local_build.rglob("*") if f.is_file() and _should_upload(f)
    )
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Uploading {len(files)} files...", total=None)
        for local_file in files:
            rel = local_file.relative_to(local_build)
            remote_file = f"{upload_path}/{rel.as_posix()}"

            # Ensure the parent directory exists on the remote side.
            if rel.parent != Path("."):
                ws.workspace.mkdirs(f"{upload_path}/{rel.parent.as_posix()}")

            # Binary files (wheels) must use RAW; everything else AUTO.
            fmt = ImportFormat.RAW if local_file.suffix == ".whl" else ImportFormat.AUTO
            ws.workspace.upload(
                remote_file, local_file.read_bytes(), format=fmt, overwrite=True
            )
            progress.update(task, description=f"Uploaded {rel}")

    console.print(f"[green]Upload complete:[/] {upload_path}")
    return deploy_path  # /Workspace/... form required by the Apps deploy API


# ---------------------------------------------------------------------------
# App lifecycle management for bench-apx
# ---------------------------------------------------------------------------


def ensure_apx_app_exists(ws: WorkspaceClient) -> None:
    """Create the bench-apx app if it does not already exist.

    Uses no_compute=True so the app is registered without starting compute —
    compute comes up automatically when the first deployment is triggered.
    We intentionally do NOT wait for ACTIVE here: the app will be STOPPED
    after creation with no_compute=True, which is expected.
    """
    try:
        ws.apps.get(APX_APP_NAME)
        console.print(f"[dim]App '{APX_APP_NAME}' already exists.[/]")
    except Exception:
        console.print(f"[bold blue]Creating app '{APX_APP_NAME}'...[/]")
        # Fire-and-forget: don't call create_and_wait — that waiter polls for
        # ACTIVE but with no_compute=True the app stays STOPPED until deploy.
        ws.apps.create(
            App(name=APX_APP_NAME, description="Benchmark: APX + asyncio"),
            no_compute=True,
        )
        console.print(f"[green]App '{APX_APP_NAME}' created.[/]")


def _wait_no_pending_deployment(ws: WorkspaceClient, timeout: float) -> None:
    """Block until bench-apx has no in-progress deployment.

    start() internally kicks off the last active deployment; the API rejects a
    new deploy() with 400 until that pending deployment settles.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        info = ws.apps.get(APX_APP_NAME)
        pending = info.pending_deployment
        if pending is None:
            return
        state = pending.status.state if pending.status else None
        if state != AppDeploymentState.IN_PROGRESS:
            return
        console.print(
            f"[dim]  waiting for pending deployment to settle (state={state!r})...[/]"
        )
        time.sleep(5)
    console.print(
        f"[yellow]Warning:[/] pending deployment did not clear within {timeout}s, proceeding."
    )


def deploy_apx_app(
    ws: WorkspaceClient, source_code_path: str, timeout: float = 600.0
) -> None:
    """Ensure bench-apx compute is ACTIVE, then deploy new source code.

    Lifecycle model:
      - Compute cycle (compute_status.state: ComputeState):
          STOPPED → start() → ACTIVE
          ACTIVE  → already up, deploy directly
      - Deployment cycle (app_status.state: ApplicationState):
          deploy() hot-swaps source code while compute stays ACTIVE.
          No stop/start needed around a redeploy.
    """
    tdelta = datetime.timedelta(seconds=timeout)

    info = ws.apps.get(APX_APP_NAME)
    compute_state: ComputeState | None = (
        info.compute_status.state if info.compute_status else None
    )
    app_state_str = repr(info.app_status.state) if info.app_status else "?"
    console.print(
        f"\n[bold blue]bench-apx:[/] compute={compute_state!r}  app={app_state_str}"
    )

    if compute_state == ComputeState.STOPPED:
        console.print(f"[bold blue]Starting '{APX_APP_NAME}' compute...[/]")

        def _on_start(app: App) -> None:
            c = repr(app.compute_status.state) if app.compute_status else "?"
            a = repr(app.app_status.state) if app.app_status else "?"
            console.print(f"[dim]  compute={c}  app={a}[/]")

        ws.apps.start(APX_APP_NAME).result(timeout=tdelta, callback=_on_start)
        console.print(f"[dim]'{APX_APP_NAME}' compute is ACTIVE.[/]")
    elif compute_state == ComputeState.ACTIVE:
        console.print(f"[dim]Compute already ACTIVE — skipping start.[/]")
    elif compute_state in (ComputeState.STARTING, ComputeState.UPDATING):
        compute_state_str = repr(compute_state)
        console.print(f"[dim]Compute is {compute_state_str} — waiting for ACTIVE...[/]")
        ws.apps.wait_get_app_active(APX_APP_NAME, timeout=tdelta)
    else:
        console.print(f"[red]Error:[/] Unexpected compute state {compute_state!r}")
        raise typer.Exit(1)

    # Deploy new source. The app is RUNNING at this point; deploy() hot-swaps
    # the source code and restarts the app process without touching compute.
    #
    # start() internally kicks off the previous deployment; the API rejects a
    # new deploy() until that lock clears. The lock may not appear in
    # pending_deployment immediately (timing gap), so we retry on the specific
    # BadRequest rather than trying to predict the exact moment it clears.
    console.print(
        f"[bold blue]Deploying '{APX_APP_NAME}' from {source_code_path}...[/]"
    )
    deployment = AppDeployment(
        source_code_path=source_code_path,
        mode=AppDeploymentMode.SNAPSHOT,
        command=APX_DEPLOY_COMMAND,
        env_vars=APX_DEPLOY_ENV_VARS,
    )

    _DEPLOY_RETRY_INTERVAL = 10
    _deploy_deadline = time.monotonic() + timeout
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Deploying {APX_APP_NAME}...", total=None)

        def _on_update(dep: AppDeployment) -> None:
            status = dep.status.state.value if dep.status and dep.status.state else "?"
            progress.update(
                task, description=f"Deploying {APX_APP_NAME}... (state={status})"
            )

        while True:
            try:
                ws.apps.deploy(APX_APP_NAME, deployment).result(
                    timeout=tdelta, callback=_on_update
                )
                break
            except BadRequest as exc:
                if "active deployment in progress" not in str(exc).lower():
                    raise
                if time.monotonic() >= _deploy_deadline:
                    raise
                pending = ws.apps.get(APX_APP_NAME).pending_deployment
                state = (
                    pending.status.state.value
                    if pending and pending.status and pending.status.state
                    else "unknown"
                )
                progress.update(
                    task,
                    description=f"Waiting for active deployment to clear (state={state})...",
                )
                time.sleep(_DEPLOY_RETRY_INTERVAL)

        progress.update(task, description=f"[green]{APX_APP_NAME} deployed!")

    console.print(f"[green]Deployment complete:[/] {APX_APP_NAME}")


def wait_for_apx_app_active(ws: WorkspaceClient, timeout: float = 600.0) -> None:
    """Poll bench-apx status until RUNNING."""
    deadline = time.monotonic() + timeout
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(
            f"Waiting for {APX_APP_NAME} to be ACTIVE...", total=None
        )
        while time.monotonic() < deadline:
            app = ws.apps.get(APX_APP_NAME)
            app_state: ApplicationState | None = (
                app.app_status.state if app.app_status else None
            )
            if app_state == ApplicationState.RUNNING:
                progress.update(task, description=f"[green]{APX_APP_NAME} is ACTIVE!")
                return
            progress.update(
                task, description=f"Waiting for {APX_APP_NAME}... (state={app_state!r})"
            )
            time.sleep(10)

    console.print(
        f"[red]Error:[/] {APX_APP_NAME} did not become ACTIVE within {timeout}s"
    )
    raise typer.Exit(1)
