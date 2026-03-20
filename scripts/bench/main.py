# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13", "httpx>=0.27", "pydantic>=2", "typer>=0.15", "databricks-sdk>=0.74"]
# ///
"""APX benchmark tool — Databricks Apps deployment + remote bencher.

Usage:
    uv run scripts/bench/main.py -p PROFILE build
    uv run scripts/bench/main.py -p PROFILE deploy
    uv run scripts/bench/main.py -p PROFILE bench --name run1 -d 30s -c 100
    uv run scripts/bench/main.py -p PROFILE list
    uv run scripts/bench/main.py -p PROFILE report --name run1
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

# Global state set by the app-level callback.
_profile: str = "apx"

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.apps import AppAccessControlRequest, AppPermissionLevel
from databricks.sdk.service.postgres import (
    Role,
    RoleIdentityType,
    RoleMembershipRole,
    RoleRoleSpec,
)

import sys as _sys

_sys.path.insert(0, str(Path(__file__).parent))
from app_deploy import (  # noqa: E402
    DATABRICKS_APPS,
    LOCAL_CONTAINER,
    LOCAL_DOCKERFILE,
    LOCAL_IMAGE,
    assemble_databricks_apps,
    build_apx_wheel,
    deploy_apx_app,
    ensure_apx_app_exists,
    upload_apx_app,
    wait_for_apx_app_active,
)


@app.callback()
def _main(
    profile: str = typer.Option(
        "apx", "-p", "--profile", help="Databricks CLI profile"
    ),
) -> None:
    """APX benchmark tool — Databricks Apps deployment + remote bencher."""
    global _profile
    _profile = profile
    console.print(f"[bold blue]Using Databricks CLI profile:[/] {_profile}")


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
    Scenario(
        name="create_item",
        method="POST",
        path="/api/items",
        body={"name": "bench-item", "price": 9.99, "tags": ["test"]},
    ),
]

# DATABRICKS_APPS imported from app_deploy.

# Apps that are benchmarkable targets (excludes the bencher itself).
BENCHABLE_APPS = {
    "bench_uvicorn": "bench-uvicorn",
    "bench_granian": "bench-granian",
    "bench_apx": "bench-apx",
}

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
        capture_output=True,
        text=True,
        check=False,
        cwd=PROJECT_ROOT,
    )
    msg_result = subprocess.run(
        ["git", "log", "-1", "--format=%s"],
        capture_output=True,
        text=True,
        check=False,
        cwd=PROJECT_ROOT,
    )
    return hash_result.stdout.strip(), msg_result.stdout.strip()


# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------


def check_build_tools() -> None:
    """Verify maturin is available."""
    if not shutil.which("maturin"):
        console.print(
            "[red]Error:[/] 'maturin' not found. Install with: pip install maturin"
        )
        raise typer.Exit(1)


def check_databricks_cli() -> None:
    """Verify databricks CLI is available (needed for bundle deploy/run)."""
    if not shutil.which("databricks"):
        console.print("[red]Error:[/] 'databricks' not found. Please install it.")
        raise typer.Exit(1)


def get_workspace_client(profile: str):
    """Get a Databricks WorkspaceClient for the given profile."""
    return WorkspaceClient(profile=profile)


# ---------------------------------------------------------------------------
# Databricks auth & URL helpers
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


def wait_for_app_active(
    ws: WorkspaceClient, app_name: str, timeout: float = 600.0
) -> None:
    """Poll app status via SDK until RUNNING."""
    deadline = time.monotonic() + timeout
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task(f"Waiting for {app_name} to be ACTIVE...", total=None)
        while time.monotonic() < deadline:
            app = ws.apps.get(app_name)
            state = (
                app.app_status.state.value
                if app.app_status and app.app_status.state
                else "?"
            )
            if state == "RUNNING":
                progress.update(task, description=f"[green]{app_name} is ACTIVE!")
                return
            progress.update(
                task, description=f"Waiting for {app_name}... (state={state})"
            )
            time.sleep(10)

    console.print(f"[red]Error:[/] {app_name} did not become ACTIVE within {timeout}s")
    raise typer.Exit(1)


# ---------------------------------------------------------------------------
# Databricks deployment helpers
# ---------------------------------------------------------------------------
# _stamp_wheel, build_apx_wheel, assemble_databricks_apps, DOCKER_IMAGE,
# LOCAL_IMAGE, LOCAL_CONTAINER, LOCAL_DOCKERFILE all live in app_deploy.py.


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
        console.print(
            f"[yellow]Warning:[/] bundle run {resource_key} returned non-zero (may already be running)."
        )


# ---------------------------------------------------------------------------
# Permissions
# ---------------------------------------------------------------------------


def grant_bencher_permissions(ws: WorkspaceClient) -> None:
    """Grant the bencher app's service principal CAN_USE on all benchable apps."""
    bencher_app = ws.apps.get("bench-bencher")
    # service_principal_name in the ACL API expects the client_id (UUID),
    # not the display name.
    sp_client_id = bencher_app.service_principal_client_id
    if not sp_client_id:
        console.print(
            "[yellow]Warning:[/] Could not find bencher service principal client ID — skip permission grant."
        )
        return

    console.print(
        f"\n[bold blue]Granting CAN_USE to bencher SP ({sp_client_id}) on benchable apps...[/]"
    )
    for app_name in BENCHABLE_APPS.values():
        try:
            result = ws.apps.update_permissions(
                app_name=app_name,
                access_control_list=[
                    AppAccessControlRequest(
                        service_principal_name=sp_client_id,
                        permission_level=AppPermissionLevel.CAN_USE,
                    )
                ],
            )
            # Verify SP is in the resulting ACL.
            granted = any(
                acr.service_principal_name == sp_client_id
                for acr in (result.access_control_list or [])
            )
            if granted:
                console.print(f"  [green]Granted:[/] CAN_USE on {app_name}")
            else:
                console.print(
                    f"  [yellow]Warning:[/] CAN_USE grant on {app_name} not reflected in ACL"
                )
        except Exception as exc:
            console.print(
                f"  [yellow]Warning:[/] Failed to set permission on {app_name}: {exc}"
            )


def grant_bencher_pg_access(ws: WorkspaceClient, project_id: str = "bench-pg") -> None:
    """Grant the bencher app's SP a superuser role on the Lakebase project."""
    bencher_app = ws.apps.get("bench-bencher")
    sp_client_id = bencher_app.service_principal_client_id
    if not sp_client_id:
        console.print(
            "[yellow]Warning:[/] No bencher SP client ID — skip PG role grant."
        )
        return

    # Discover the default branch (typically "production").
    branches = list(ws.postgres.list_branches(parent=f"projects/{project_id}"))
    if not branches:
        console.print(
            f"[yellow]Warning:[/] No branches found for project {project_id} — skip PG role grant."
        )
        return
    branch = branches[0].name
    role_id = f"sp-{sp_client_id[:8]}"
    console.print(
        f"[bold blue]Granting PG superuser role to bencher SP on {branch}...[/]"
    )
    assert branch, f"Branch is required, got {branch}"
    try:
        ws.postgres.create_role(
            parent=branch,
            role=Role(
                spec=RoleRoleSpec(
                    identity_type=RoleIdentityType.SERVICE_PRINCIPAL,
                    membership_roles=[RoleMembershipRole.DATABRICKS_SUPERUSER],
                    postgres_role=sp_client_id,
                )
            ),
            role_id=role_id,
        )
        console.print(f"  [green]Granted:[/] PG role {role_id} on {branch}")
    except Exception as exc:
        # Role may already exist from a previous deploy.
        console.print(f"  [yellow]Warning:[/] PG role grant: {exc}")


# ---------------------------------------------------------------------------
# Bencher HTTP client helpers
# ---------------------------------------------------------------------------


def _bencher_headers(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}


def _get_bencher_url(ws: WorkspaceClient) -> str:
    return get_app_url(ws, "bench-bencher")


def _poll_run(
    bencher_url: str, token: str, run_id: str, poll_interval: float = 5.0
) -> dict:
    """Poll GET /api/benchmarks/{id} until completed or failed. Returns final response."""
    headers = _bencher_headers(token)
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("Waiting for benchmark...", total=None)
        while True:
            resp = httpx.get(
                f"{bencher_url}/api/benchmarks/{run_id}",
                headers=headers,
                timeout=30.0,
            )
            resp.raise_for_status()
            data = resp.json()
            status = data["status"]
            prog = data.get("progress", {})
            current = prog.get("current", "")
            completed = prog.get("completed", 0)
            total = prog.get("total", 0)

            if status == "completed":
                progress.update(task, description="[green]Benchmark completed!")
                return data
            elif status == "failed":
                progress.update(task, description="[red]Benchmark failed!")
                return data
            else:
                desc = f"[{completed}/{total}] {current}" if total else current
                progress.update(task, description=desc)

            time.sleep(poll_interval)


# ---------------------------------------------------------------------------
# Report printing (reused from original — operates on report dict)
# ---------------------------------------------------------------------------


def _safe_ratio(a: float, b: float) -> float | None:
    """Compute a/b, returning None if b is zero."""
    return round(a / b, 4) if b else None


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
                    row.append(f"{1 / r:.2f}x")
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
        lat = (
            report["scenarios"]
            .get(sname, {})
            .get("comparison", {})
            .get("latency_ms", {})
        )
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

        # Rust-side breakdown table (if available).
        rust_bd = pdata.get("rust_breakdown", {})
        if rust_bd:
            console.print(f"\n[bold yellow]Rust pipeline breakdown: {env_name}[/]")
            rt = Table(show_header=True, header_style="bold")
            rt.add_column("Path", style="dim")
            rt.add_column("N", justify="right")
            rt.add_column("total p50", justify="right")
            rt.add_column("GIL p50", justify="right")
            rt.add_column("scope p50", justify="right")
            rt.add_column("app_call p50", justify="right")
            rt.add_column("drive p50", justify="right")
            rt.add_column("resp_wait p50", justify="right")
            rt.add_column("steps avg", justify="right")

            for path, rs in rust_bd.items():
                rt.add_row(
                    path,
                    str(rs["count"]),
                    f"{rs.get('total_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('gil_acquire_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('scope_build_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('app_call_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('drive_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('response_wait_us_p50', 0):.0f}\u00b5s",
                    f"{rs.get('steps_avg', 0):.1f}",
                )
            console.print(rt)

    # Profiling ratios.
    prof_ratios = report.get("profiling_ratios", {})
    if prof_ratios:
        console.print("\n[bold magenta]Profiling ratios (p50):[/]")
        for path, ratios in prof_ratios.items():
            parts = [f"{k}={v:.2f}x" for k, v in ratios.items() if v is not None]
            if parts:
                console.print(f"  {path}: {', '.join(parts)}")


def _print_report(report: dict) -> None:
    """Print report tables from a report dict."""
    # Derive env_names and scenario_names from the report.
    scenarios_section = report.get("scenarios", {})
    scenario_names = sorted(scenarios_section.keys())

    env_names_set: set[str] = set()
    for sdata in scenarios_section.values():
        env_names_set.update(
            sdata.get("comparison", {}).get("throughput_rps", {}).keys()
        )
    env_names = sorted(env_names_set)

    if scenario_names:
        _print_comparison_tables(report, env_names, scenario_names)
    if report.get("profiling"):
        _print_profiling_tables(report, env_names)


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
def deploy() -> None:
    """Deploy Databricks bundle and start apps."""
    check_databricks_cli()

    ws = get_workspace_client(_profile)

    # bench-apx is deployed via SDK (DABs lack telemetry support).
    bundle_resource_keys = [k for k in DATABRICKS_APPS if k != "bench_apx"]
    bundle_app_names = [DATABRICKS_APPS[k] for k in bundle_resource_keys]

    deploy_databricks_bundle(_profile)

    for resource_key in bundle_resource_keys:
        run_databricks_app(_profile, resource_key)

    for app_name in bundle_app_names:
        wait_for_app_active(ws, app_name)

    # ── bench-apx: custom SDK-based deploy ───────────────────────────────────
    ensure_apx_app_exists(ws)
    source_code_path = upload_apx_app(ws)
    deploy_apx_app(ws, source_code_path)
    wait_for_apx_app_active(ws)

    # Print app URLs.
    console.print("\n[bold green]All apps ACTIVE:[/]")
    for resource_key, app_name in DATABRICKS_APPS.items():
        url = get_app_url(ws, app_name)
        console.print(f"  [cyan]{app_name}:[/] {url}")

    # Grant bencher SP CAN_USE on all benchable apps.
    grant_bencher_permissions(ws)

    # Grant bencher SP access to the Lakebase PostgreSQL project.
    grant_bencher_pg_access(ws)


@app.command()
def bench(
    name: str = typer.Option(..., help="Run name"),
    duration: str = typer.Option(
        "10s", "-d", "--duration", help="Duration per scenario (oha -z)"
    ),
    connections: int = typer.Option(
        100, "-c", "--connections", help="Concurrent connections"
    ),
    warmup: int = typer.Option(1000, help="Number of warmup requests"),
    do_profile: bool = typer.Option(
        False, "--profile-asgi", help="Also run profiling pass"
    ),
    detached: bool = typer.Option(
        False, "--detached", help="Start and return immediately without polling"
    ),
    results_dir: Path = typer.Option(DEFAULT_RESULTS, "--results-dir"),
) -> None:
    """Start a remote benchmark via the bencher server."""
    ws = get_workspace_client(_profile)
    bencher_url = _get_bencher_url(ws)
    token = get_databricks_token(ws)

    console.print(f"[bold blue]Bencher:[/] {bencher_url}")

    # Build environments map: env_name → app_name.
    environments = {KEY_TO_ENV[k]: v for k, v in BENCHABLE_APPS.items()}

    config = {
        "name": name,
        "environments": environments,
        "duration": duration,
        "connections": connections,
        "warmup_requests": warmup,
        "profile": do_profile,
    }

    # POST /api/benchmarks
    headers = _bencher_headers(token)
    resp = httpx.post(
        f"{bencher_url}/api/benchmarks",
        json=config,
        headers=headers,
        timeout=30.0,
    )
    if resp.status_code != 202:
        console.print(
            f"[red]Error:[/] Failed to start benchmark: {resp.status_code} {resp.text}"
        )
        raise typer.Exit(1)

    run_data = resp.json()
    run_id = run_data["id"]
    console.print(f"[green]Started:[/] run_id={run_id}  name={name}")

    if detached:
        console.print(
            "[dim]Detached mode — use 'list' to check status, 'report --name' to fetch results.[/]"
        )
        return

    # Poll until completed/failed.
    final = _poll_run(bencher_url, token, run_id)

    if final["status"] == "failed":
        console.print(
            f"[red]Benchmark failed:[/] {final.get('error_message', 'unknown error')}"
        )
        raise typer.Exit(1)

    # Download report.
    resp = httpx.get(
        f"{bencher_url}/api/benchmarks/{run_id}/report",
        headers=headers,
        timeout=30.0,
    )
    if resp.status_code != 200:
        console.print("[yellow]Warning:[/] Could not download report.")
        raise typer.Exit(1)

    report = resp.json()

    # Save report locally.
    run_dir = results_dir / name
    run_dir.mkdir(parents=True, exist_ok=True)
    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2))
    console.print(f"[bold green]Report saved:[/] {report_path}")

    _print_report(report)


@app.command("list")
def list_runs() -> None:
    """List benchmark runs from the remote bencher server."""
    ws = get_workspace_client(_profile)
    bencher_url = _get_bencher_url(ws)
    token = get_databricks_token(ws)

    headers = _bencher_headers(token)
    resp = httpx.get(f"{bencher_url}/api/benchmarks", headers=headers, timeout=30.0)
    resp.raise_for_status()
    runs = resp.json()

    if not runs:
        console.print("[dim]No benchmark runs found.[/]")
        return

    table = Table(title="Benchmark Runs")
    table.add_column("ID", style="dim", max_width=12)
    table.add_column("Name", style="cyan")
    table.add_column("Status")
    table.add_column("Mode")
    table.add_column("Progress")
    table.add_column("Created")

    for r in runs:
        status = r["status"]
        style = {"completed": "green", "failed": "red", "running": "yellow"}.get(
            status, ""
        )
        prog = r.get("progress", {})
        prog_str = f"{prog.get('completed', 0)}/{prog.get('total', 0)}"
        table.add_row(
            r["id"][:12],
            r["name"],
            f"[{style}]{status}[/{style}]" if style else status,
            r["mode"],
            prog_str,
            r["created_at"][:19],
        )

    console.print(table)


def _resolve_run_id_by_name(bencher_url: str, token: str, name: str) -> str | None:
    """Query the server for the latest run with the given name. Returns run_id or None."""
    headers = _bencher_headers(token)
    resp = httpx.get(
        f"{bencher_url}/api/benchmarks",
        params={"name": name},
        headers=headers,
        timeout=30.0,
    )
    resp.raise_for_status()
    runs = resp.json()
    if not runs:
        return None
    # First result is the latest (server orders by created_at desc).
    return runs[0]["id"]


def _fetch_and_print_report(
    bencher_url: str,
    token: str,
    run_id: str,
    *,
    save_dir: Path | None = None,
) -> None:
    """Download report from server, optionally save locally, and print tables."""
    headers = _bencher_headers(token)
    resp = httpx.get(
        f"{bencher_url}/api/benchmarks/{run_id}/report",
        headers=headers,
        timeout=30.0,
    )
    if resp.status_code == 404:
        console.print(f"[red]Error:[/] Report not available for run {run_id}")
        raise typer.Exit(1)
    resp.raise_for_status()

    report_data = resp.json()

    if save_dir:
        save_dir.mkdir(parents=True, exist_ok=True)
        report_path = save_dir / "report.json"
        report_path.write_text(json.dumps(report_data, indent=2))
        console.print(f"[bold green]Report saved:[/] {report_path}")

    _print_report(report_data)


@app.command()
def report(
    name: str = typer.Option(None, help="Run name (server or local)"),
    run_id: str = typer.Option(None, "--id", help="Run ID (download from server)"),
    results_dir: Path = typer.Option(DEFAULT_RESULTS, "--results-dir"),
    scenarios: Path = typer.Option(DEFAULT_SCENARIOS, help="Path to scenarios.json"),
) -> None:
    """Display report — by run name or run ID."""
    if run_id:
        # Fetch by explicit ID.
        ws = get_workspace_client(_profile)
        bencher_url = _get_bencher_url(ws)
        token = get_databricks_token(ws)
        _fetch_and_print_report(bencher_url, token, run_id)

    elif name:
        # Try local first, then server.
        run_dir = results_dir / name
        report_path = run_dir / "report.json"

        if report_path.exists():
            report_data = json.loads(report_path.read_text())
            _print_report(report_data)
        else:
            # Try server by name.
            ws = get_workspace_client(_profile)
            bencher_url = _get_bencher_url(ws)
            token = get_databricks_token(ws)
            resolved_id = _resolve_run_id_by_name(bencher_url, token, name)

            if resolved_id:
                console.print(f"[dim]Resolved '{name}' → {resolved_id[:12]}[/]")
                _fetch_and_print_report(
                    bencher_url,
                    token,
                    resolved_id,
                    save_dir=run_dir,
                )
            elif run_dir.exists():
                # Legacy filesystem-based report generation.
                sys.path.insert(0, str(BENCH_DIR))
                from profile_analysis import analyze_records, load_records  # noqa: F401

                _generate_report_legacy(run_dir, scenarios)
            else:
                console.print(
                    f"[red]Error:[/] Run '{name}' not found locally or on server"
                )
                raise typer.Exit(1)
    else:
        console.print("[red]Error:[/] Provide --name or --id")
        raise typer.Exit(1)


def _generate_report_legacy(run_dir: Path, scenarios_path: Path) -> None:
    """Legacy report generation from filesystem results (backward compat)."""
    sys.path.insert(0, str(BENCH_DIR))
    from profile_analysis import analyze_records, load_records

    meta_raw = json.loads((run_dir / "meta.json").read_text())
    meta = RunMeta(**meta_raw)
    scenario_list = [Scenario(**s) for s in json.loads(scenarios_path.read_text())]

    env_names = [e.name for e in meta.environments]
    scenario_meta = {
        s.name: {"method": s.method, "path": s.path} for s in scenario_list
    }

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

    summary: dict[str, dict] = {}
    for i, a in enumerate(env_names):
        for b in env_names[i + 1 :]:
            label = f"{a}_vs_{b}"
            tp_ratios = []
            for sname in scenario_names:
                r = (
                    scenarios_section.get(sname, {})
                    .get("comparison", {})
                    .get("ratios", {})
                    .get(label, {})
                )
                if r.get("throughput") is not None:
                    tp_ratios.append(r["throughput"])
            summary.setdefault("avg_throughput_ratio", {})[label] = (
                round(sum(tp_ratios) / len(tp_ratios), 4) if tp_ratios else None
            )
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

    report_data = {
        "meta": meta_raw,
        "scenarios": scenarios_section,
        "profiling": profiling_section,
        "profiling_ratios": profiling_ratios,
        "summary": summary,
    }

    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report_data, indent=2))
    console.print(f"\n[bold green]Report written:[/] {report_path}")

    _print_report(report_data)


# ---------------------------------------------------------------------------
# Local Docker runner
# ---------------------------------------------------------------------------

local_app = typer.Typer(help="Run APX benchmark app locally via Docker")
app.add_typer(local_app, name="local")


def _default_cpuset(cpus: float) -> str:
    """Pin to ceil(cpus) cores starting from 0 so nproc inside the container
    matches the actual CPU budget and thread pools size correctly."""
    import math

    n = max(1, math.ceil(cpus))
    return ",".join(str(i) for i in range(n))


@local_app.command()
def start(
    port: int = typer.Option(8000, "--port", help="Host port to expose"),
    workers: int = typer.Option(2, "--workers", "-w", help="Number of APX workers"),
    cpus: float = typer.Option(2.0, "--cpus", help="CPU limit for the container"),
    cpuset_cpus: str = typer.Option(
        "",
        "--cpuset-cpus",
        help="Pin to specific cores (e.g. '0,1'). Auto-derived from --cpus if empty.",
    ),
    memory: str = typer.Option(
        "6g", "--memory", "-m", help="Memory limit (e.g. 512m, 2g, 6g)"
    ),
    max_concurrent: int = typer.Option(
        0,
        "--max-concurrent",
        help="Max concurrent requests per worker (0 = framework default 256)",
    ),
    build_image: bool = typer.Option(
        True, "--build/--no-build", help="Build Docker image before starting"
    ),
) -> None:
    """Build the Docker image and start the APX bench app locally."""
    build_dir = DATABRICKS_DIR / ".build" / "bench-apx"
    if not build_dir.exists():
        console.print("[red]Error:[/] Build directory not found. Run 'build' first.")
        raise typer.Exit(1)

    if build_image:
        console.print("[bold blue]Building Docker image...[/]")
        result = subprocess.run(
            [
                "docker",
                "build",
                "-f",
                str(LOCAL_DOCKERFILE),
                "-t",
                LOCAL_IMAGE,
                str(build_dir),
            ],
            check=False,
        )
        if result.returncode != 0:
            console.print("[red]Error:[/] Docker build failed")
            raise typer.Exit(1)
        console.print(f"[green]Image built:[/] {LOCAL_IMAGE}")

    subprocess.run(
        ["docker", "rm", "-f", LOCAL_CONTAINER],
        capture_output=True,
        check=False,
    )

    pinned = cpuset_cpus or _default_cpuset(cpus)
    console.print(
        f"[bold blue]Starting container on port {port} "
        f"(cpus={cpus}, cpuset-cpus={pinned}, memory={memory}, swap=disabled)...[/]"
    )
    result = subprocess.run(
        [
            "docker",
            "run",
            "-d",
            "--platform",
            "linux/amd64",
            "--name",
            LOCAL_CONTAINER,
            "--cpus",
            str(cpus),
            "--cpuset-cpus",
            pinned,
            "--memory",
            memory,
            "--memory-swap",
            memory,
            "-p",
            f"{port}:8000",
            "-e",
            "APX_BENCH_SERVER=apx",
            "-e",
            "APX_BENCH_PROFILE=1",
            "-e",
            "APX_PERF=1",
            LOCAL_IMAGE,
            "apx",
            "serve",
            "app.main",
            "--host",
            "0.0.0.0",
            "--workers",
            str(workers),
            *(["--max-concurrent", str(max_concurrent)] if max_concurrent > 0 else []),
        ],
        check=False,
    )
    if result.returncode != 0:
        console.print("[red]Error:[/] Failed to start container")
        raise typer.Exit(1)

    console.print(f"[bold green]APX running at http://localhost:{port}[/]")


@local_app.command()
def stop() -> None:
    """Stop the locally running APX bench app."""
    result = subprocess.run(
        ["docker", "rm", "-f", LOCAL_CONTAINER],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0:
        console.print(f"[green]Container '{LOCAL_CONTAINER}' stopped and removed.[/]")
    else:
        console.print("[yellow]No running container found.[/]")


if __name__ == "__main__":
    app()
