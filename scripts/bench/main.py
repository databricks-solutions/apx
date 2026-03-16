# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13", "httpx>=0.27", "pydantic>=2", "typer>=0.15", "databricks-sdk>=0.74"]
# ///
"""APX benchmark tool — Databricks Apps deployment.

Usage:
    uv run scripts/bench/main.py build
    uv run scripts/bench/main.py deploy  -p PROFILE
    uv run scripts/bench/main.py bench   -p PROFILE --name run1 -d 30s -c 100
    uv run scripts/bench/main.py profile -p PROFILE --name run1 -d 10s -c 100
    uv run scripts/bench/main.py report  --name run1
"""
from __future__ import annotations

import datetime
import json
import shutil
import subprocess
import sys
import time
from enum import Enum
from pathlib import Path

import httpx
import typer
from pydantic import BaseModel
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.table import Table

console = Console()
app = typer.Typer(help="APX benchmark tool")
from databricks.sdk import WorkspaceClient

# ---------------------------------------------------------------------------
# Pydantic models
# ---------------------------------------------------------------------------


class ServerType(str, Enum):
    UVICORN = "uvicorn"
    APX = "apx"
    GRANIAN = "granian"


class Scheduler(str, Enum):
    ASYNCIO = "asyncio"
    UVLOOP = "uvloop"


class Environment(BaseModel):
    """A server configuration to benchmark."""

    name: str
    server: ServerType
    scheduler: Scheduler
    workers: int = 2
    description: str = ""


class Scenario(BaseModel):
    """An HTTP scenario to benchmark."""

    name: str
    method: str
    path: str
    body: dict | None = None


class ScenarioResult(BaseModel):
    """Parsed oha result for one scenario + one environment."""

    environment: str
    scenario: str
    requests_per_sec: float
    latency_p50_ms: float
    latency_p90_ms: float
    latency_p99_ms: float
    success_rate: float
    total_requests: int

    @classmethod
    def from_oha_json(
        cls, env_name: str, scenario_name: str, raw: dict
    ) -> ScenarioResult | None:
        summary = raw.get("summary", {})
        percentiles = raw.get("latencyPercentiles", {})

        def _f(d: dict, key: str) -> float:
            v = d.get(key)
            return float(v) if v is not None else 0.0

        success_rate = _f(summary, "successRate")
        if success_rate == 0.0:
            return None

        return cls(
            environment=env_name,
            scenario=scenario_name,
            requests_per_sec=_f(summary, "requestsPerSec"),
            latency_p50_ms=_f(percentiles, "p50") * 1000,
            latency_p90_ms=_f(percentiles, "p90") * 1000,
            latency_p99_ms=_f(percentiles, "p99") * 1000,
            success_rate=success_rate,
            total_requests=int(_f(summary, "total")),
        )


class RunMeta(BaseModel):
    """Metadata for a benchmark run."""

    name: str
    timestamp: datetime.datetime
    commit_hash: str
    commit_message: str
    duration: str
    connections: int
    warmup_requests: int
    mode: str
    environments: list[Environment]


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DIR = Path(__file__).resolve().parent
DATABRICKS_DIR = BENCH_DIR / "databricks"
DEFAULT_SCENARIOS = BENCH_DIR / "scenarios.json"
DEFAULT_RESULTS = BENCH_DIR / "results"

CROSS_TARGET = "x86_64-unknown-linux-gnu"

PROFILE_SCENARIOS = [
    Scenario(name="echo", method="GET", path="/api/echo"),
    Scenario(name="health", method="GET", path="/api/health"),
    Scenario(name="get_item", method="GET", path="/api/items/1"),
    Scenario(name="list_items", method="GET", path="/api/items"),
    Scenario(name="create_item", method="POST", path="/api/items",
             body={"name": "bench-item", "price": 9.99, "tags": ["test"]}),
]

DATABRICKS_APPS = {
    "bench_uvicorn": "bench-uvicorn",
    "bench_granian": "bench-granian",
    "bench_apx": "bench-apx",
}

DATABRICKS_ENVS = [
    Environment(
        name="uvicorn",
        server=ServerType.UVICORN,
        scheduler=Scheduler.UVLOOP,
        workers=2,
        description="Uvicorn + uvloop + httptools",
    ),
    Environment(
        name="granian",
        server=ServerType.GRANIAN,
        scheduler=Scheduler.UVLOOP,
        workers=2,
        description="Granian + ASGI + uvloop",
    ),
    Environment(
        name="apx",
        server=ServerType.APX,
        scheduler=Scheduler.ASYNCIO,
        workers=2,
        description="APX + asyncio",
    ),
]

KEY_TO_ENV = {
    "bench_uvicorn": "uvicorn",
    "bench_granian": "granian",
    "bench_apx": "apx",
}

# ---------------------------------------------------------------------------
# Git helpers
# ---------------------------------------------------------------------------


def get_git_info() -> tuple[str, str]:
    """Return (commit_hash, commit_message)."""
    hash_result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True, text=True, check=False, cwd=PROJECT_ROOT,
    )
    msg_result = subprocess.run(
        ["git", "log", "-1", "--format=%s"],
        capture_output=True, text=True, check=False, cwd=PROJECT_ROOT,
    )
    return hash_result.stdout.strip(), msg_result.stdout.strip()


# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------


def check_build_tools() -> None:
    """Verify maturin is available."""
    if not shutil.which("maturin"):
        console.print("[red]Error:[/] 'maturin' not found. Install with: pip install maturin")
        raise typer.Exit(1)


def check_databricks_cli() -> None:
    """Verify databricks CLI is available (needed for bundle deploy/run)."""
    if not shutil.which("databricks"):
        console.print("[red]Error:[/] 'databricks' not found. Please install it.")
        raise typer.Exit(1)


def get_workspace_client(profile: str):
    """Get a Databricks WorkspaceClient for the given profile."""
    return WorkspaceClient(profile=profile)


def check_oha() -> None:
    """Verify oha is available."""
    if not shutil.which("oha"):
        console.print("[red]Error:[/] 'oha' not found. Please install it.")
        raise typer.Exit(1)


# ---------------------------------------------------------------------------
# Run directory helpers
# ---------------------------------------------------------------------------


def ensure_run_dir(
    name: str,
    results_dir: Path,
    *,
    mode: str,
    duration: str,
    connections: int,
    warmup_requests: int,
) -> Path:
    """Create run dir + meta.json on first use, update mode on subsequent calls."""
    run_dir = results_dir / name
    meta_path = run_dir / "meta.json"

    if run_dir.exists() and meta_path.exists():
        # Update mode if needed (e.g. bench -> bench+profile).
        meta_raw = json.loads(meta_path.read_text())
        existing_mode = meta_raw.get("mode", "")
        if mode not in existing_mode:
            if existing_mode:
                meta_raw["mode"] = f"{existing_mode}+{mode}"
            else:
                meta_raw["mode"] = mode
            meta_path.write_text(json.dumps(meta_raw, indent=2))
        console.print(f"[bold green]Run:[/] {name} → {run_dir} (existing)")
        return run_dir

    run_dir.mkdir(parents=True, exist_ok=True)

    commit_hash, commit_message = get_git_info()
    meta = RunMeta(
        name=name,
        timestamp=datetime.datetime.now(datetime.timezone.utc),
        commit_hash=commit_hash,
        commit_message=commit_message,
        duration=duration,
        connections=connections,
        warmup_requests=warmup_requests,
        mode=mode,
        environments=DATABRICKS_ENVS,
    )

    meta_path.write_text(json.dumps(meta.model_dump(mode="json"), indent=2))
    console.print(f"[bold green]Run:[/] {name} → {run_dir}")
    console.print(f"[dim]Commit: {commit_hash[:12]} — {commit_message}[/]")
    return run_dir


def resolve_app_urls(ws: WorkspaceClient) -> dict[str, str]:
    """Fetch all 3 app URLs from Databricks via SDK."""
    urls = {}
    for key, name in DATABRICKS_APPS.items():
        urls[key] = get_app_url(ws, name)
    return urls


# ---------------------------------------------------------------------------
# Wheel build (maturin cross-compilation)
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
    new_di_name = re.sub(r"-[\d][^-]*\.dist-info$", f"-{new_version}.dist-info", old_name)
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
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
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

    # cleanup
    shutil.rmtree(tmp_dir)
    if wheel_path != new_wheel:
        wheel_path.unlink()

    return new_wheel


def build_apx_wheel(dest_dir: Path) -> Path:
    """Cross-compile APX wheel for linux/amd64 using maturin + zig."""
    import re

    # Stub the agent binary — crates/core/build.rs copies it and resources.rs
    # embeds it via include_bytes!(). The bench wheel only uses `apx serve`,
    # never the agent, so a zero-byte stub is fine.
    agent_stub = PROJECT_ROOT / ".bins" / "agent" / "apx-agent-linux-x64"
    agent_stub.parent.mkdir(parents=True, exist_ok=True)
    agent_stub.touch()
    console.print(f"[dim]Stubbed agent binary:[/] {agent_stub}")

    console.print("\n[bold blue]Building APX wheel via maturin...[/]")
    dest_dir.mkdir(parents=True, exist_ok=True)
    for old in dest_dir.glob("apx-*.whl"):
        old.unlink()
    result = subprocess.run(
        [
            "maturin", "build", "--release",
            "--target", CROSS_TARGET,
            "--zig",
            "-i", "python3.11",
            "--out", str(dest_dir),
        ],
        cwd=str(PROJECT_ROOT),
        check=False,
    )
    if result.returncode != 0:
        console.print("[red]Error:[/] maturin build failed")
        raise typer.Exit(1)

    wheels = sorted(dest_dir.glob("apx-*.whl"), key=lambda p: p.stat().st_mtime, reverse=True)
    assert wheels, "No APX wheel found after maturin build"

    # Read base version from the built wheel's metadata, stamp with timestamp.
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
# Databricks assembly & deployment
# ---------------------------------------------------------------------------


def assemble_databricks_apps(wheel_path: Path | None) -> None:
    """Copy shared app code + per-app configs → .build/{app-name}/."""
    build_dir = DATABRICKS_DIR / ".build"
    app_src = BENCH_DIR / "app"

    for app_name in DATABRICKS_APPS.values():
        app_build = build_dir / app_name
        if app_build.exists():
            shutil.rmtree(app_build)
        app_build.mkdir(parents=True)

        # Copy shared app code.
        shutil.copytree(app_src, app_build / "app")

        # Copy per-app requirements.txt.
        src_reqs = DATABRICKS_DIR / "apps" / app_name / "requirements.txt"
        dest_reqs = app_build / "requirements.txt"
        shutil.copy2(src_reqs, dest_reqs)

        # For bench-apx, copy wheel and append to requirements.txt.
        if app_name == "bench-apx" and wheel_path is not None:
            shutil.copy2(wheel_path, app_build / wheel_path.name)
            with open(dest_reqs, "a") as f:
                f.write(f"./{wheel_path.name}\n")

        console.print(f"[green]Assembled:[/] {app_build}")


def deploy_databricks_bundle(profile: str) -> None:
    """Deploy the Databricks bundle."""
    console.print("\n[bold blue]Deploying Databricks bundle...[/]")
    result = subprocess.run(
        ["databricks", "bundle", "deploy", "-p", profile],
        cwd=str(DATABRICKS_DIR),
        check=False,
    )
    if result.returncode != 0:
        console.print("[red]Error:[/] databricks bundle deploy failed.")
        raise typer.Exit(1)
    console.print("[green]Bundle deployed.[/]")


def run_databricks_app(profile: str, resource_key: str) -> None:
    """Run (start) a Databricks App via bundle run."""
    console.print(f"  [cyan]Starting {resource_key}...[/]")
    result = subprocess.run(
        ["databricks", "bundle", "run", resource_key, "-p", profile],
        cwd=str(DATABRICKS_DIR),
        check=False,
    )
    if result.returncode != 0:
        console.print(f"[yellow]Warning:[/] bundle run {resource_key} returned non-zero (may already be running).")


# ---------------------------------------------------------------------------
# Databricks auth & polling
# ---------------------------------------------------------------------------


def get_databricks_token(ws: WorkspaceClient) -> str:
    """Get a fresh Databricks access token via SDK."""
    headers = ws.config.authenticate()
    auth = headers.get("Authorization", "")
    if not auth.startswith("Bearer "):
        console.print("[red]Error:[/] Failed to get Databricks token")
        raise typer.Exit(1)
    return auth.removeprefix("Bearer ")


def get_app_url(ws: WorkspaceClient, app_name: str) -> str:
    """Get the URL for a Databricks App via SDK."""
    app = ws.apps.get(app_name)
    url = app.url or ""
    if not url:
        console.print(f"[red]Error:[/] No URL found for {app_name}")
        raise typer.Exit(1)
    return url.rstrip("/")


def wait_for_app_active(ws: WorkspaceClient, app_name: str, timeout: float = 600.0) -> None:
    """Poll app status via SDK until RUNNING."""
    deadline = time.monotonic() + timeout
    with Progress(
        SpinnerColumn(), TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Waiting for {app_name} to be ACTIVE...", total=None)
        while time.monotonic() < deadline:
            app = ws.apps.get(app_name)
            state = app.app_status.state.value if app.app_status and app.app_status.state else "?"
            if state == "RUNNING":
                progress.update(task, description=f"[green]{app_name} is ACTIVE!")
                return
            progress.update(task, description=f"Waiting for {app_name}... (state={state})")
            time.sleep(10)

    console.print(f"[red]Error:[/] {app_name} did not become ACTIVE within {timeout}s")
    raise typer.Exit(1)


# ---------------------------------------------------------------------------
# Remote health, warmup, oha, profiling
# ---------------------------------------------------------------------------


def wait_for_health(url: str, token: str, timeout: float = 120.0) -> None:
    """Poll /api/health on a remote Databricks App."""
    health_url = f"{url}/api/health"
    deadline = time.monotonic() + timeout
    headers = {"Authorization": f"Bearer {token}"}

    with Progress(
        SpinnerColumn(), TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Waiting for {url} health...", total=None)
        while time.monotonic() < deadline:
            try:
                resp = httpx.get(health_url, headers=headers, timeout=10.0)
                if resp.status_code == 200:
                    progress.update(task, description=f"[green]{url} healthy!")
                    return
            except httpx.HTTPError:
                pass
            time.sleep(5)

    console.print(f"[red]Error:[/] {url} did not become healthy within {timeout}s")
    raise typer.Exit(1)


def run_warmup(url: str, token: str, warmup_requests: int) -> None:
    """Send warmup requests to a remote Databricks App."""
    if warmup_requests <= 0:
        return
    console.print(f"  [dim]Warming up {url} with {warmup_requests} requests...[/]")
    cmd = [
        "oha",
        "--no-tui",
        "-n", str(warmup_requests),
        "-c", str(min(warmup_requests, 50)),
        "-H", f"Authorization: Bearer {token}",
        f"{url}/api/health",
    ]
    subprocess.run(cmd, capture_output=True, check=False)


def run_oha(
    scenario: Scenario,
    url: str,
    token: str,
    duration: str,
    connections: int,
    output_path: Path,
) -> bool:
    """Run oha against a remote Databricks App. Returns True on success."""
    cmd = [
        "oha",
        "--output-format", "json",
        "--no-tui",
        "-z", duration,
        "-c", str(connections),
        "-m", scenario.method,
        "-H", f"Authorization: Bearer {token}",
    ]

    if scenario.body is not None:
        cmd.extend(["-d", json.dumps(scenario.body)])
        cmd.extend(["-T", "application/json"])

    cmd.append(f"{url}{scenario.path}")

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(
            f"  [yellow]Warning:[/] oha failed for {scenario.name}: {result.stderr[:200]}"
        )
        return False

    output_path.write_text(result.stdout)
    return True


def extract_profiling(url: str, token: str, dest: Path) -> None:
    """Download profiling JSONL from a remote Databricks App."""
    headers = {"Authorization": f"Bearer {token}"}
    try:
        resp = httpx.get(f"{url}/api/profile/dump", headers=headers, timeout=30.0)
        if resp.status_code == 200:
            dest.write_text(resp.text)
            console.print(f"  [green]Profile data:[/] {dest}")
        else:
            console.print(f"  [yellow]No profile data:[/] {resp.status_code}")
    except httpx.HTTPError as exc:
        console.print(f"  [red]Error fetching profile:[/] {exc}")


def reset_profiling(url: str, token: str) -> None:
    """Reset profiling data on a remote Databricks App."""
    headers = {"Authorization": f"Bearer {token}"}
    try:
        httpx.delete(f"{url}/api/profile/reset", headers=headers, timeout=10.0)
    except httpx.HTTPError:
        pass


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------


def _safe_ratio(a: float, b: float) -> float | None:
    """Compute a/b, returning None if b is zero."""
    return round(a / b, 4) if b else None


def generate_report(
    run_dir: Path,
    scenarios: list[Scenario],
) -> None:
    """Load all results, compute ratios, write report.json, print terminal tables."""
    sys.path.insert(0, str(BENCH_DIR))
    from profile_analysis import analyze_records, load_records

    meta_raw = json.loads((run_dir / "meta.json").read_text())
    meta = RunMeta(**meta_raw)

    env_names = [e.name for e in meta.environments]
    scenario_meta = {s.name: {"method": s.method, "path": s.path} for s in scenarios}

    # ── Discover throughput results from disk ──
    all_oha: dict[str, dict[str, dict]] = {}
    all_scenario_names: set[str] = set()
    for env_name in env_names:
        all_oha[env_name] = {}
        env_dir = run_dir / "environments" / env_name
        if not env_dir.exists():
            continue
        for json_file in sorted(env_dir.glob("*.json")):
            sname = json_file.stem
            all_oha[env_name][sname] = json.loads(json_file.read_text())
            all_scenario_names.add(sname)

    scenario_names = sorted(all_scenario_names)

    # ── Build scenarios section ──
    scenarios_section: dict[str, dict] = {}
    for sname in scenario_names:
        smeta = scenario_meta.get(sname, {})
        results_raw: dict[str, dict] = {}
        throughput_rps: dict[str, float] = {}
        latency_ms: dict[str, dict] = {}

        for env_name in env_names:
            raw = all_oha[env_name].get(sname)
            if raw is None:
                continue
            results_raw[env_name] = raw
            parsed = ScenarioResult.from_oha_json(env_name, sname, raw)
            if parsed:
                throughput_rps[env_name] = round(parsed.requests_per_sec, 1)
                latency_ms[env_name] = {
                    "p50": round(parsed.latency_p50_ms, 2),
                    "p90": round(parsed.latency_p90_ms, 2),
                    "p99": round(parsed.latency_p99_ms, 2),
                }

        # Compute pairwise ratios.
        ratios: dict[str, dict] = {}
        for i, a in enumerate(env_names):
            for b in env_names[i + 1 :]:
                label = f"{a}_vs_{b}"
                a_tp = throughput_rps.get(a, 0)
                b_tp = throughput_rps.get(b, 0)
                a_lat = latency_ms.get(a, {})
                b_lat = latency_ms.get(b, {})
                ratios[label] = {
                    "throughput": _safe_ratio(a_tp, b_tp),
                    "latency_p50": _safe_ratio(
                        a_lat.get("p50", 0), b_lat.get("p50", 0)
                    ),
                    "latency_p99": _safe_ratio(
                        a_lat.get("p99", 0), b_lat.get("p99", 0)
                    ),
                }

        scenarios_section[sname] = {
            **smeta,
            "results": results_raw,
            "comparison": {
                "throughput_rps": throughput_rps,
                "latency_ms": latency_ms,
                "ratios": ratios,
            },
        }

    # ── Load profiling data ──
    profiling_section: dict[str, dict] = {}
    profile_dir = run_dir / "profile"
    if profile_dir.exists():
        for env_name in env_names:
            jsonl_path = profile_dir / f"{env_name}.jsonl"
            if not jsonl_path.exists():
                continue
            info, reqs = load_records(jsonl_path)
            stats = analyze_records(reqs)

            endpoints: dict[str, dict] = {}
            for path, s in stats.items():
                endpoints[path] = {
                    "count": s["count"],
                    "handler_us": {
                        "p50": round(s["handler_p50_us"], 1),
                        "p99": round(s["handler_p99_us"], 1),
                        "avg": round(s["handler_avg_us"], 1),
                    },
                    "send_us": {
                        "p50": round(s["send_p50_us"], 1),
                        "p99": round(s["send_p99_us"], 1),
                        "avg": round(s["send_avg_us"], 1),
                    },
                    "recv_us": {
                        "p50": round(s["recv_p50_us"], 1),
                        "p99": round(s["recv_p99_us"], 1),
                        "avg": round(s["recv_avg_us"], 1),
                    },
                    "total_us": {
                        "p50": round(s["total_p50_us"], 1),
                        "p99": round(s["total_p99_us"], 1),
                        "avg": round(s["total_avg_us"], 1),
                    },
                    "recv_calls_avg": round(s["recv_calls_avg"], 1),
                    "send_calls_avg": round(s["send_calls_avg"], 1),
                }

            profiling_section[env_name] = {
                "info": {
                    "loop": info.get("loop", "?") if info else "?",
                    "python": info.get("python", "?") if info else "?",
                    "pid": info.get("pid", "?") if info else "?",
                },
                "endpoints": endpoints,
            }

    # ── Compute profiling ratios ──
    profiling_ratios: dict[str, dict] = {}
    all_paths: set[str] = set()
    for env_name in env_names:
        if env_name in profiling_section:
            all_paths.update(profiling_section[env_name]["endpoints"].keys())

    for path in sorted(all_paths):
        path_ratios: dict[str, float | None] = {}
        for i, a in enumerate(env_names):
            for b in env_names[i + 1 :]:
                a_ep = profiling_section.get(a, {}).get("endpoints", {}).get(path)
                b_ep = profiling_section.get(b, {}).get("endpoints", {}).get(path)
                if a_ep and b_ep:
                    label = f"{a}_vs_{b}"
                    path_ratios[f"handler_p50_{label}"] = _safe_ratio(
                        a_ep["handler_us"]["p50"], b_ep["handler_us"]["p50"]
                    )
                    path_ratios[f"send_p50_{label}"] = _safe_ratio(
                        a_ep["send_us"]["p50"], b_ep["send_us"]["p50"]
                    )
        if path_ratios:
            profiling_ratios[path] = path_ratios

    # ── Compute summary ──
    summary: dict[str, dict] = {}
    for i, a in enumerate(env_names):
        for b in env_names[i + 1 :]:
            label = f"{a}_vs_{b}"

            # Average throughput ratio.
            tp_ratios = []
            for sname in scenario_names:
                r = scenarios_section.get(sname, {}).get("comparison", {}).get("ratios", {}).get(label, {})
                if r.get("throughput") is not None:
                    tp_ratios.append(r["throughput"])
            summary.setdefault("avg_throughput_ratio", {})[label] = (
                round(sum(tp_ratios) / len(tp_ratios), 4) if tp_ratios else None
            )

            # Average profiling ratios.
            handler_rs, send_rs = [], []
            for path in sorted(all_paths):
                pr = profiling_ratios.get(path, {})
                h = pr.get(f"handler_p50_{label}")
                s = pr.get(f"send_p50_{label}")
                if h is not None:
                    handler_rs.append(h)
                if s is not None:
                    send_rs.append(s)
            summary.setdefault("avg_handler_p50_ratio", {})[label] = (
                round(sum(handler_rs) / len(handler_rs), 4) if handler_rs else None
            )
            summary.setdefault("avg_send_p50_ratio", {})[label] = (
                round(sum(send_rs) / len(send_rs), 4) if send_rs else None
            )

    # ── Assemble report ──
    report = {
        "meta": meta_raw,
        "scenarios": scenarios_section,
        "profiling": profiling_section,
        "profiling_ratios": profiling_ratios,
        "summary": summary,
    }

    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2))
    console.print(f"\n[bold green]Report written:[/] {report_path}")

    _print_comparison_tables(report, env_names, scenario_names)
    if profiling_section:
        _print_profiling_tables(report, env_names)


# ---------------------------------------------------------------------------
# Terminal printing
# ---------------------------------------------------------------------------


def _print_comparison_tables(
    report: dict,
    env_names: list[str],
    scenario_names: list[str],
) -> None:
    """Print throughput + latency comparison tables."""
    # Throughput table.
    table = Table(title="Throughput (requests/sec)")
    table.add_column("Scenario", style="cyan")
    for name in env_names:
        table.add_column(name, justify="right")
    # Add ratio columns: each env vs the first env.
    if len(env_names) > 1:
        for name in env_names[1:]:
            table.add_column(f"{name}/{env_names[0]}", justify="right")

    for sname in scenario_names:
        comp = report["scenarios"].get(sname, {}).get("comparison", {})
        tp = comp.get("throughput_rps", {})
        ratios = comp.get("ratios", {})
        row = [sname]
        for name in env_names:
            row.append(f"{tp.get(name, 0):,.0f}")
        if len(env_names) > 1:
            for name in env_names[1:]:
                label = f"{env_names[0]}_vs_{name}"
                r = ratios.get(label, {}).get("throughput")
                if r is not None and r != 0:
                    row.append(f"{1/r:.2f}x")
                else:
                    row.append("N/A")
        table.add_row(*row)

    console.print(table)

    # Latency table.
    table = Table(title="Latency p50 / p99 (ms)")
    table.add_column("Scenario", style="cyan")
    for name in env_names:
        table.add_column(name, justify="right")

    for sname in scenario_names:
        lat = report["scenarios"].get(sname, {}).get("comparison", {}).get("latency_ms", {})
        row = [sname]
        for name in env_names:
            l = lat.get(name, {})
            row.append(f"{l.get('p50', 0):.1f} / {l.get('p99', 0):.1f}")
        table.add_row(*row)

    console.print(table)

    # Summary.
    s = report.get("summary", {})
    console.print("\n[bold]Summary (averages across all scenarios):[/]")
    for i, a in enumerate(env_names):
        for b in env_names[i + 1 :]:
            label = f"{a}_vs_{b}"
            tp = s.get("avg_throughput_ratio", {}).get(label)
            hp = s.get("avg_handler_p50_ratio", {}).get(label)
            sp = s.get("avg_send_p50_ratio", {}).get(label)
            parts = []
            if tp is not None:
                parts.append(f"throughput={tp:.2f}x")
            if hp is not None:
                parts.append(f"handler_p50={hp:.2f}x")
            if sp is not None:
                parts.append(f"send_p50={sp:.2f}x")
            if parts:
                console.print(f"  {label}: {', '.join(parts)}")


def _print_profiling_tables(report: dict, env_names: list[str]) -> None:
    """Print profiling breakdown tables."""
    profiling = report.get("profiling", {})

    for env_name in env_names:
        pdata = profiling.get(env_name)
        if not pdata:
            continue

        info = pdata.get("info", {})
        console.print(
            f"\n[bold cyan]Profiling: {env_name}[/]"
            f"  loop={info.get('loop', '?')}  python={info.get('python', '?')}"
        )

        table = Table(show_header=True, header_style="bold")
        table.add_column("Path", style="dim")
        table.add_column("N", justify="right")
        table.add_column("total p50", justify="right")
        table.add_column("handler p50", justify="right")
        table.add_column("send p50", justify="right")
        table.add_column("recv/send calls", justify="right")

        for path, ep in pdata.get("endpoints", {}).items():
            table.add_row(
                path,
                str(ep["count"]),
                f"{ep['total_us']['p50']:.0f}\u00b5s",
                f"{ep['handler_us']['p50']:.0f}\u00b5s",
                f"{ep['send_us']['p50']:.0f}\u00b5s",
                f"{ep['recv_calls_avg']:.1f}/{ep['send_calls_avg']:.1f}",
            )

        console.print(table)

    # Profiling ratios.
    prof_ratios = report.get("profiling_ratios", {})
    if prof_ratios:
        console.print("\n[bold magenta]Profiling ratios (p50):[/]")
        for path, ratios in prof_ratios.items():
            parts = [f"{k}={v:.2f}x" for k, v in ratios.items() if v is not None]
            if parts:
                console.print(f"  {path}: {', '.join(parts)}")


# ---------------------------------------------------------------------------
# Typer commands
# ---------------------------------------------------------------------------


@app.command()
def build() -> None:
    """Cross-compile APX wheel and assemble app directories."""
    check_build_tools()

    wheel_dest = DATABRICKS_DIR / ".build" / ".wheels"
    wheel_path = build_apx_wheel(wheel_dest)

    assemble_databricks_apps(wheel_path)
    console.print("\n[bold green]Build complete.[/]")


@app.command()
def deploy(
    profile: str = typer.Option("DEFAULT", "-p", "--profile", help="Databricks CLI profile"),
) -> None:
    """Deploy Databricks bundle and start apps."""
    check_databricks_cli()

    deploy_databricks_bundle(profile)

    for resource_key in DATABRICKS_APPS:
        run_databricks_app(profile, resource_key)

    ws = get_workspace_client(profile)
    for app_name in DATABRICKS_APPS.values():
        wait_for_app_active(ws, app_name)

    # Print app URLs.
    console.print("\n[bold green]All apps ACTIVE:[/]")
    for resource_key, app_name in DATABRICKS_APPS.items():
        url = get_app_url(ws, app_name)
        console.print(f"  [cyan]{app_name}:[/] {url}")


@app.command()
def bench(
    name: str = typer.Option(..., help="Run name (results stored under results/<name>/)"),
    profile: str = typer.Option("DEFAULT", "-p", "--profile", help="Databricks CLI profile"),
    duration: str = typer.Option("10s", "-d", "--duration", help="Duration per scenario (oha -z)"),
    connections: int = typer.Option(100, "-c", "--connections", help="Concurrent connections"),
    warmup: int = typer.Option(1000, help="Number of warmup requests"),
    scenarios: Path = typer.Option(DEFAULT_SCENARIOS, help="Path to scenarios.json"),
    results_dir: Path = typer.Option(DEFAULT_RESULTS, "--results-dir"),
    no_report: bool = typer.Option(False, "--no-report", help="Skip report generation"),
) -> None:
    """Run throughput benchmarks against live Databricks Apps."""
    check_oha()

    ws = get_workspace_client(profile)
    scenario_list = [Scenario(**s) for s in json.loads(scenarios.read_text())]
    run_dir = ensure_run_dir(
        name, results_dir,
        mode="bench", duration=duration, connections=connections, warmup_requests=warmup,
    )

    app_urls = resolve_app_urls(ws)
    for key, url in app_urls.items():
        console.print(f"  [cyan]{DATABRICKS_APPS[key]}:[/] {url}")

    # Health check + warmup.
    token = get_databricks_token(ws)
    for url in app_urls.values():
        wait_for_health(url, token)
    for url in app_urls.values():
        run_warmup(url, token, warmup)

    # Run benchmarks for each app.
    for resource_key, url in app_urls.items():
        env_name = KEY_TO_ENV[resource_key]
        console.print(f"\n[bold magenta]{'=' * 50}[/]")
        console.print(f"[bold magenta]Databricks: {env_name}[/]")
        console.print(f"[bold magenta]{'=' * 50}[/]")

        # Refresh token before each environment pass.
        token = get_databricks_token(ws)

        env_dir = run_dir / "environments" / env_name
        env_dir.mkdir(parents=True, exist_ok=True)

        for scenario in scenario_list:
            console.print(
                f"  [cyan]Running:[/] {scenario.name} ({scenario.method} {scenario.path})"
            )
            output_path = env_dir / f"{scenario.name}.json"
            ok = run_oha(scenario, url, token, duration, connections, output_path)
            if ok:
                console.print(f"  [green]Done:[/] {output_path}")
            else:
                console.print(f"  [yellow]Skipped:[/] {scenario.name}")

    if not no_report:
        generate_report(run_dir, scenario_list)


@app.command("profile")
def profile_cmd(
    name: str = typer.Option(..., help="Run name (results stored under results/<name>/)"),
    profile: str = typer.Option("DEFAULT", "-p", "--profile", help="Databricks CLI profile"),
    duration: str = typer.Option("10s", "-d", "--duration", help="Duration per profiling scenario"),
    connections: int = typer.Option(100, "-c", "--connections", help="Concurrent connections"),
    warmup: int = typer.Option(1000, help="Number of warmup requests"),
    results_dir: Path = typer.Option(DEFAULT_RESULTS, "--results-dir"),
    no_report: bool = typer.Option(False, "--no-report", help="Skip report generation"),
) -> None:
    """Run profiling benchmarks against live Databricks Apps."""
    check_oha()

    ws = get_workspace_client(profile)
    run_dir = ensure_run_dir(
        name, results_dir,
        mode="profile", duration=duration, connections=connections, warmup_requests=warmup,
    )

    app_urls = resolve_app_urls(ws)
    for key, url in app_urls.items():
        console.print(f"  [cyan]{DATABRICKS_APPS[key]}:[/] {url}")

    # Health check + warmup.
    token = get_databricks_token(ws)
    for url in app_urls.values():
        wait_for_health(url, token)
    for url in app_urls.values():
        run_warmup(url, token, warmup)

    # Run profiling for each app.
    prof_dir = run_dir / "profile"
    prof_dir.mkdir(parents=True, exist_ok=True)

    for resource_key, url in app_urls.items():
        env_name = KEY_TO_ENV[resource_key]
        console.print(f"\n[bold magenta]{'=' * 50}[/]")
        console.print(f"[bold magenta]Profiling: {env_name}[/]")
        console.print(f"[bold magenta]{'=' * 50}[/]")

        # Refresh token before each environment pass.
        token = get_databricks_token(ws)

        reset_profiling(url, token)

        for scenario in PROFILE_SCENARIOS:
            console.print(f"  [cyan]Profiling:[/] {scenario.name}")
            run_oha(
                scenario, url, token, duration,
                connections, prof_dir / f"_oha_{env_name}_{scenario.name}.json",
            )

        extract_profiling(url, token, prof_dir / f"{env_name}.jsonl")

    if not no_report:
        scenario_list = [Scenario(**s) for s in json.loads(DEFAULT_SCENARIOS.read_text())]
        generate_report(run_dir, scenario_list)


@app.command()
def report(
    name: str = typer.Option(..., help="Run name"),
    results_dir: Path = typer.Option(DEFAULT_RESULTS, "--results-dir"),
    scenarios: Path = typer.Option(DEFAULT_SCENARIOS, help="Path to scenarios.json"),
) -> None:
    """Generate report from existing results."""
    run_dir = results_dir / name
    if not run_dir.exists():
        console.print(f"[red]Error:[/] Run '{name}' not found at {run_dir}")
        raise typer.Exit(1)

    scenario_list = [Scenario(**s) for s in json.loads(scenarios.read_text())]
    generate_report(run_dir, scenario_list)


if __name__ == "__main__":
    app()
