# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13", "httpx>=0.27", "pydantic>=2"]
# ///
"""APX benchmark orchestrator.

Usage:
    uv run scripts/bench/run_bench.py --name <run-name> [OPTIONS]
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
import time
from enum import Enum
from pathlib import Path

import httpx
from pydantic import BaseModel
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.table import Table

console = Console()

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

    # Build config.
    dockerfile: Path
    context: Path
    build_args: dict[str, str] = {}
    image_tag: str

    # Runtime config.
    runtime_env: dict[str, str] = {}
    description: str = ""

    def container_cmd(self) -> list[str]:
        """Build the CMD for docker run."""
        if self.server == ServerType.UVICORN:
            return [
                "uvicorn", "app.main:app",
                "--host", "0.0.0.0", "--port", "8000",
                "--workers", str(self.workers),
                "--loop", "uvloop", "--http", "httptools",
            ]
        elif self.server == ServerType.GRANIAN:
            loop = "uvloop" if self.scheduler == Scheduler.UVLOOP else "asyncio"
            return [
                "granian", "--interface", "asgi",
                "--loop", loop,
                "app.main:app",
                "--host", "0.0.0.0", "--port", "8000",
                "--workers", str(self.workers),
            ]
        return [
            "apx", "serve", "app.main",
            "--host", "0.0.0.0", "--port", "8000",
            "--workers", str(self.workers),
        ]


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
    profiling_duration: str
    connections: int
    warmup_requests: int
    container_limits: dict[str, str]
    mode: str
    environments: list[Environment]


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DIR = Path(__file__).resolve().parent
DOCKER_DIR = BENCH_DIR / "docker"
DEFAULT_SCENARIOS = BENCH_DIR / "scenarios.json"
DEFAULT_RESULTS = BENCH_DIR / "results"

# Pre-built environment configs.

ENV_UVICORN = Environment(
    name="uvicorn",
    server=ServerType.UVICORN,
    scheduler=Scheduler.UVLOOP,
    workers=2,
    dockerfile=DOCKER_DIR / "Dockerfile.uvicorn",
    context=BENCH_DIR,
    image_tag="bench-uvicorn",
    description="Uvicorn + uvloop + httptools",
)

ENV_APX_ASYNCIO = Environment(
    name="apx-asyncio",
    server=ServerType.APX,
    scheduler=Scheduler.ASYNCIO,
    workers=2,
    dockerfile=DOCKER_DIR / "Dockerfile.apx",
    context=PROJECT_ROOT,
    image_tag="bench-apx",
    description="APX + asyncio",
)

ENV_APX_UVLOOP = Environment(
    name="apx-uvloop",
    server=ServerType.APX,
    scheduler=Scheduler.UVLOOP,
    workers=2,
    dockerfile=DOCKER_DIR / "Dockerfile.apx",
    context=PROJECT_ROOT,
    runtime_env={"APX_SCHEDULER": "uvloop"},
    image_tag="bench-apx",
    description="APX + uvloop",
)

ENV_GRANIAN = Environment(
    name="granian",
    server=ServerType.GRANIAN,
    scheduler=Scheduler.ASYNCIO,
    workers=2,
    dockerfile=DOCKER_DIR / "Dockerfile.granian",
    context=BENCH_DIR,
    image_tag="bench-granian",
    description="Granian + ASGI interface",
)

ENV_GRANIAN_UVLOOP = Environment(
    name="granian-uvloop",
    server=ServerType.GRANIAN,
    scheduler=Scheduler.UVLOOP,
    workers=2,
    dockerfile=DOCKER_DIR / "Dockerfile.granian",
    context=BENCH_DIR,
    image_tag="bench-granian",
    description="Granian + ASGI + uvloop",
)

PROFILE_SCENARIOS = [
    Scenario(name="echo", method="GET", path="/api/echo"),
    Scenario(name="health", method="GET", path="/api/health"),
    Scenario(name="get_item", method="GET", path="/api/items/1"),
    Scenario(name="list_items", method="GET", path="/api/items"),
    Scenario(name="create_item", method="POST", path="/api/items",
             body={"name": "bench-item", "price": 9.99, "tags": ["test"]}),
]

SWEEP_WORKERS = [1, 2]
SWEEP_TOKIO_THREADS = [1, 2, 4]
SWEEP_CONNECTIONS = [10, 50, 100, 200]

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="APX benchmark orchestrator")
    p.add_argument("--name", required=True, help="Run name (results stored under results/<name>/)")
    p.add_argument("-d", "--duration", default="30s", help="Duration per scenario (oha -z)")
    p.add_argument("-c", "--connections", type=int, default=100, help="Concurrent connections")
    p.add_argument("--cpus", default="2", help="CPU limit for containers")
    p.add_argument("--memory", default="4g", help="Memory limit for containers")
    p.add_argument("--port", type=int, default=8000, help="Host port to map")
    p.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS)
    p.add_argument("--scenarios", type=Path, default=DEFAULT_SCENARIOS)
    p.add_argument("--skip-build", action="store_true", help="Skip docker build")
    p.add_argument(
        "--server",
        choices=["uvicorn", "apx", "granian", "both"],
        default="both",
        help="Which server to benchmark (for default and profile modes)",
    )
    p.add_argument("--no-report", action="store_true", help="Skip report generation")
    p.add_argument(
        "--tokio-threads",
        type=int,
        default=None,
        help="Set TOKIO_WORKER_THREADS in APX container",
    )
    p.add_argument(
        "--warmup",
        type=int,
        default=1000,
        help="Number of warmup requests before benchmarking (default: 1000)",
    )
    p.add_argument(
        "--sweep",
        action="store_true",
        help="Run sweep mode: echo scenario across worker/thread/connection matrix",
    )
    p.add_argument(
        "--profile",
        action="store_true",
        help="Run profiling: measures Python-level per-request timing",
    )
    p.add_argument(
        "--profile-duration",
        default="15s",
        help="Duration for profiling load (default: 15s)",
    )
    p.add_argument(
        "--compare",
        action="store_true",
        help="Run 3-way comparison (uvicorn vs granian vs APX+asyncio)",
    )
    return p.parse_args()


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
# Run setup
# ---------------------------------------------------------------------------


def setup_run(
    args: argparse.Namespace,
    mode: str,
    envs: list[Environment],
) -> tuple[Path, RunMeta]:
    """Create run directory and write meta.json."""
    run_dir = args.results_dir / args.name
    if run_dir.exists():
        console.print(f"[red]Error:[/] Run '{args.name}' already exists at {run_dir}")
        console.print("[dim]Use a different --name or delete the existing run.[/]")
        sys.exit(1)

    run_dir.mkdir(parents=True)

    commit_hash, commit_message = get_git_info()
    meta = RunMeta(
        name=args.name,
        timestamp=datetime.datetime.now(datetime.timezone.utc),
        commit_hash=commit_hash,
        commit_message=commit_message,
        duration=args.duration,
        profiling_duration=args.profile_duration,
        connections=args.connections,
        warmup_requests=args.warmup,
        container_limits={"cpus": args.cpus, "memory": args.memory},
        mode=mode,
        environments=envs,
    )

    # Write meta with relative paths for portability.
    meta_dict = meta.model_dump(mode="json")
    for env in meta_dict.get("environments", []):
        for key in ("dockerfile", "context"):
            if key in env:
                try:
                    env[key] = str(Path(env[key]).relative_to(PROJECT_ROOT))
                except ValueError:
                    pass
    (run_dir / "meta.json").write_text(json.dumps(meta_dict, indent=2))
    console.print(f"[bold green]Run:[/] {args.name} → {run_dir}")
    console.print(f"[dim]Commit: {commit_hash[:12]} — {commit_message}[/]")
    return run_dir, meta


# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------


def check_prerequisites() -> None:
    """Verify docker and oha are available."""
    for tool in ("docker", "oha"):
        if not shutil.which(tool):
            console.print(f"[red]Error:[/] '{tool}' not found. Please install it.")
            sys.exit(1)

    result = subprocess.run(
        ["docker", "info"], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        console.print("[red]Error:[/] Docker daemon is not running.")
        sys.exit(1)


# ---------------------------------------------------------------------------
# Docker helpers
# ---------------------------------------------------------------------------


def build_image(env: Environment) -> None:
    """Build a Docker image for an environment."""
    console.print(f"\n[bold blue]Building {env.image_tag}...[/]")
    cmd = [
        "docker", "build",
        "-f", str(env.dockerfile),
        "-t", env.image_tag,
    ]
    for k, v in env.build_args.items():
        cmd.extend(["--build-arg", f"{k}={v}"])
    cmd.append(str(env.context))
    env_os = {**os.environ, "DOCKER_BUILDKIT": "1"}
    result = subprocess.run(cmd, env=env_os, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to build {env.image_tag}.")
        sys.exit(1)
    console.print(f"[green]Built {env.image_tag}[/]")


def build_images(envs: list[Environment]) -> None:
    """Build Docker images, deduplicating by tag."""
    built: set[str] = set()
    for env in envs:
        if env.image_tag not in built:
            build_image(env)
            built.add(env.image_tag)


def remove_stale_container(name: str) -> None:
    """Remove container if it exists (from a previous failed run)."""
    subprocess.run(
        ["docker", "rm", "-f", f"bench-{name}"],
        capture_output=True, check=False,
    )


def start_container(
    env: Environment,
    port: int,
    cpus: str,
    memory: str,
    *,
    extra_env: dict[str, str] | None = None,
) -> str:
    """Start a container for an environment. Returns container ID."""
    remove_stale_container(env.name)
    cmd = [
        "docker", "run", "-d",
        "--name", f"bench-{env.name}",
        f"--cpus={cpus}",
        f"--memory={memory}",
        "-p", f"{port}:8000",
    ]

    env_vars: dict[str, str] = dict(env.runtime_env)
    if extra_env:
        env_vars.update(extra_env)
    for k, v in env_vars.items():
        cmd.extend(["-e", f"{k}={v}"])

    cmd.append(env.image_tag)
    cmd.extend(env.container_cmd())

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to start {env.name}: {result.stderr}")
        sys.exit(1)
    container_id = result.stdout.strip()
    env_str = f" env={env_vars}" if env_vars else ""
    console.print(f"[green]Started bench-{env.name}[/] ({container_id[:12]}){env_str}")
    return container_id


def stop_container(name: str) -> None:
    """Stop and remove a container."""
    subprocess.run(["docker", "stop", f"bench-{name}"], capture_output=True, check=False)
    subprocess.run(["docker", "rm", f"bench-{name}"], capture_output=True, check=False)
    console.print(f"[dim]Stopped bench-{name}[/]")


def print_container_logs(name: str) -> None:
    """Print last 20 lines of container logs (for debugging)."""
    result = subprocess.run(
        ["docker", "logs", "--tail", "20", f"bench-{name}"],
        capture_output=True, text=True, check=False,
    )
    if result.stdout:
        console.print(f"[dim]--- logs for bench-{name} ---[/]")
        console.print(result.stdout)
    if result.stderr:
        console.print(result.stderr)


# ---------------------------------------------------------------------------
# Health check & warmup
# ---------------------------------------------------------------------------


def wait_for_health(port: int, timeout: float = 30.0) -> None:
    """Poll /api/health until it responds 200."""
    url = f"http://127.0.0.1:{port}/api/health"
    deadline = time.monotonic() + timeout

    with Progress(
        SpinnerColumn(), TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("Waiting for server to be ready...", total=None)
        while time.monotonic() < deadline:
            try:
                resp = httpx.get(url, timeout=2.0)
                if resp.status_code == 200:
                    progress.update(task, description="[green]Server ready!")
                    return
            except httpx.HTTPError:
                pass
            time.sleep(0.5)

    console.print(f"[red]Error:[/] Server did not become healthy within {timeout}s")
    sys.exit(1)


def run_warmup(port: int, warmup_requests: int) -> None:
    """Send warmup requests before benchmarking."""
    if warmup_requests <= 0:
        return
    console.print(f"  [dim]Warming up with {warmup_requests} requests...[/]")
    cmd = [
        "oha",
        "--no-tui",
        "-n", str(warmup_requests),
        "-c", str(min(warmup_requests, 50)),
        f"http://127.0.0.1:{port}/api/health",
    ]
    subprocess.run(cmd, capture_output=True, check=False)


# ---------------------------------------------------------------------------
# oha runner
# ---------------------------------------------------------------------------


def run_oha(
    scenario: Scenario,
    port: int,
    duration: str,
    connections: int,
    output_path: Path,
) -> bool:
    """Run oha for a single scenario. Returns True on success."""
    cmd = [
        "oha",
        "--output-format", "json",
        "--no-tui",
        "-z", duration,
        "-c", str(connections),
        "-m", scenario.method,
    ]

    if scenario.body is not None:
        cmd.extend(["-d", json.dumps(scenario.body)])
        cmd.extend(["-T", "application/json"])

    cmd.append(f"http://127.0.0.1:{port}{scenario.path}")

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(
            f"  [yellow]Warning:[/] oha failed for {scenario.name}: {result.stderr[:200]}"
        )
        return False

    output_path.write_text(result.stdout)
    return True


# ---------------------------------------------------------------------------
# Benchmark passes
# ---------------------------------------------------------------------------


def run_throughput_pass(
    env: Environment,
    scenarios: list[Scenario],
    run_dir: Path,
    args: argparse.Namespace,
) -> None:
    """Run throughput benchmarks for one environment."""
    console.print(f"\n[bold magenta]{'=' * 50}[/]")
    console.print(f"[bold magenta]Throughput: {env.name}[/]")
    console.print(f"[bold magenta]{'=' * 50}[/]")

    env_dir = run_dir / "environments" / env.name
    env_dir.mkdir(parents=True, exist_ok=True)

    start_container(env, args.port, args.cpus, args.memory)
    try:
        wait_for_health(args.port)
        run_warmup(args.port, args.warmup)

        for scenario in scenarios:
            console.print(
                f"  [cyan]Running:[/] {scenario.name} ({scenario.method} {scenario.path})"
            )
            output_path = env_dir / f"{scenario.name}.json"
            ok = run_oha(scenario, args.port, args.duration, args.connections, output_path)
            if ok:
                console.print(f"  [green]Done:[/] {output_path}")
            else:
                console.print(f"  [yellow]Skipped:[/] {scenario.name}")
    except Exception:
        print_container_logs(env.name)
        raise
    finally:
        stop_container(env.name)


def run_profiling_pass(
    env: Environment,
    run_dir: Path,
    args: argparse.Namespace,
) -> None:
    """Run ASGI profiling for one environment."""
    console.print(f"\n[bold magenta]{'=' * 50}[/]")
    console.print(f"[bold magenta]Profiling: {env.name}[/]")
    console.print(f"[bold magenta]{'=' * 50}[/]")

    profile_dir = run_dir / "profile"
    profile_dir.mkdir(parents=True, exist_ok=True)

    start_container(
        env, args.port, args.cpus, args.memory,
        extra_env={"APX_BENCH_PROFILE": "1"},
    )
    try:
        wait_for_health(args.port)
        run_warmup(args.port, args.warmup)

        for scenario in PROFILE_SCENARIOS:
            console.print(f"  [cyan]Profiling:[/] {scenario.name}")
            run_oha(
                scenario, args.port, args.profile_duration,
                args.connections, profile_dir / f"_oha_{env.name}_{scenario.name}.json",
            )

        # Extract profiling JSONL from container.
        jsonl_path = profile_dir / f"{env.name}.jsonl"
        cp_cmd = [
            "docker", "cp",
            f"bench-{env.name}:/tmp/bench_profile.jsonl",
            str(jsonl_path),
        ]
        result = subprocess.run(cp_cmd, capture_output=True, text=True, check=False)
        if result.returncode == 0:
            console.print(f"  [green]Profile data:[/] {jsonl_path}")
        else:
            console.print(f"  [red]Error copying profile:[/] {result.stderr[:200]}")
    except Exception:
        print_container_logs(env.name)
        raise
    finally:
        stop_container(env.name)


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------


def _safe_ratio(a: float, b: float) -> float | None:
    """Compute a/b, returning None if b is zero."""
    return round(a / b, 4) if b else None


def generate_report(
    run_dir: Path,
    meta: RunMeta,
    scenarios: list[Scenario],
) -> None:
    """Load all results, compute ratios, write report.json, print terminal tables."""
    sys.path.insert(0, str(BENCH_DIR))
    from profile_analysis import analyze_records, load_records

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
        "meta": json.loads((run_dir / "meta.json").read_text()),
        "scenarios": scenarios_section,
        "profiling": profiling_section,
        "profiling_ratios": profiling_ratios,
        "summary": summary,
    }

    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2))
    console.print(f"\n[bold green]Report written:[/] {report_path}")

    # ── Terminal output ──
    if meta.mode == "sweep":
        _print_sweep_tables(report, env_names, scenario_names)
    else:
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
                # Ratio label is always first_vs_second (alphabetical by index).
                label = f"{env_names[0]}_vs_{name}"
                r = ratios.get(label, {}).get("throughput")
                # Invert: we want name/first, but stored as first/name.
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


def _print_sweep_tables(
    report: dict,
    env_names: list[str],
    scenario_names: list[str],
) -> None:
    """Print sweep results as a row-per-environment table."""
    table = Table(title="Sweep Results (req/sec)")
    table.add_column("Environment", style="cyan")
    table.add_column("Connections", justify="right")
    table.add_column("Req/sec", justify="right", style="bold")
    table.add_column("p50 (ms)", justify="right")
    table.add_column("p99 (ms)", justify="right")

    for env_name in env_names:
        for sname in scenario_names:
            comp = report["scenarios"].get(sname, {}).get("comparison", {})
            tp = comp.get("throughput_rps", {}).get(env_name)
            lat = comp.get("latency_ms", {}).get(env_name, {})
            if tp is None:
                continue

            # Parse connection count from scenario name (echo_c100 → 100).
            connections = sname.split("_c")[-1] if "_c" in sname else sname
            table.add_row(
                env_name,
                connections,
                f"{tp:,.0f}",
                f"{lat.get('p50', 0):.2f}",
                f"{lat.get('p99', 0):.2f}",
            )

    console.print(table)


# ---------------------------------------------------------------------------
# Mode: default
# ---------------------------------------------------------------------------


def _with_tokio_threads(env: Environment, threads: int) -> Environment:
    """Return a copy of env with TOKIO_WORKER_THREADS set."""
    if env.server != ServerType.APX:
        return env
    return env.model_copy(update={
        "runtime_env": {**env.runtime_env, "TOKIO_WORKER_THREADS": str(threads)},
    })


def run_default(args: argparse.Namespace) -> None:
    """Run standard benchmark: throughput for selected servers."""
    if args.server == "both":
        envs = [ENV_UVICORN, ENV_APX_ASYNCIO]
    elif args.server == "uvicorn":
        envs = [ENV_UVICORN]
    elif args.server == "granian":
        envs = [ENV_GRANIAN]
    else:
        envs = [ENV_APX_ASYNCIO]

    if args.tokio_threads is not None:
        envs = [_with_tokio_threads(e, args.tokio_threads) for e in envs]

    scenarios = [Scenario(**s) for s in json.loads(args.scenarios.read_text())]
    run_dir, meta = setup_run(args, "default", envs)

    if not args.skip_build:
        build_images(envs)

    for env in envs:
        run_throughput_pass(env, scenarios, run_dir, args)

    if not args.no_report:
        generate_report(run_dir, meta, scenarios)


# ---------------------------------------------------------------------------
# Mode: --compare
# ---------------------------------------------------------------------------


def run_compare(args: argparse.Namespace) -> None:
    """Run 3-way comparison: uvicorn vs granian vs APX (all uvloop)."""
    envs = [ENV_UVICORN, ENV_GRANIAN_UVLOOP, ENV_APX_UVLOOP]
    scenarios = [Scenario(**s) for s in json.loads(args.scenarios.read_text())]
    run_dir, meta = setup_run(args, "compare", envs)

    if not args.skip_build:
        build_images(envs)

    for env in envs:
        run_throughput_pass(env, scenarios, run_dir, args)
        run_profiling_pass(env, run_dir, args)

    if not args.no_report:
        generate_report(run_dir, meta, scenarios)


# ---------------------------------------------------------------------------
# Mode: --profile
# ---------------------------------------------------------------------------


def run_profile(args: argparse.Namespace) -> None:
    """Run profiling for selected servers."""
    if args.server == "both":
        envs = [ENV_UVICORN, ENV_APX_ASYNCIO]
    elif args.server == "uvicorn":
        envs = [ENV_UVICORN]
    else:
        envs = [ENV_APX_ASYNCIO]

    if args.tokio_threads is not None:
        envs = [_with_tokio_threads(e, args.tokio_threads) for e in envs]

    run_dir, meta = setup_run(args, "profile", envs)

    if not args.skip_build:
        build_images(envs)

    for env in envs:
        run_profiling_pass(env, run_dir, args)

    if not args.no_report:
        generate_report(run_dir, meta, list(PROFILE_SCENARIOS))


# ---------------------------------------------------------------------------
# Mode: --sweep
# ---------------------------------------------------------------------------


def run_sweep(args: argparse.Namespace) -> None:
    """Run echo scenario across a worker/thread/connection matrix."""
    echo = Scenario(name="echo", method="GET", path="/api/echo")

    server_names = ["uvicorn", "apx"] if args.server == "both" else [args.server]

    # Generate environments from the sweep matrix.
    envs: list[Environment] = []
    for server_name in server_names:
        base = ENV_APX_ASYNCIO if server_name == "apx" else ENV_UVICORN
        thread_values: list[int | None] = (
            SWEEP_TOKIO_THREADS if server_name == "apx" else [None]
        )
        for workers in SWEEP_WORKERS:
            for tokio_threads in thread_values:
                name = f"{server_name}-w{workers}"
                if tokio_threads is not None:
                    name += f"-t{tokio_threads}"

                runtime_env = dict(base.runtime_env)
                if tokio_threads is not None:
                    runtime_env["TOKIO_WORKER_THREADS"] = str(tokio_threads)

                envs.append(base.model_copy(update={
                    "name": name,
                    "workers": workers,
                    "runtime_env": runtime_env,
                }))

    # Sweep scenario names encode the connection count.
    sweep_scenarios = [
        Scenario(name=f"echo_c{c}", method="GET", path="/api/echo")
        for c in SWEEP_CONNECTIONS
    ]

    run_dir, meta = setup_run(args, "sweep", envs)

    if not args.skip_build:
        build_images(envs)

    for env in envs:
        console.print(f"\n[bold magenta]{'=' * 50}[/]")
        console.print(f"[bold magenta]Sweep: {env.name}[/]")
        console.print(f"[bold magenta]{'=' * 50}[/]")

        env_dir = run_dir / "environments" / env.name
        env_dir.mkdir(parents=True, exist_ok=True)

        start_container(env, args.port, args.cpus, args.memory)
        try:
            wait_for_health(args.port)
            run_warmup(args.port, args.warmup)

            for connections in SWEEP_CONNECTIONS:
                console.print(f"  [cyan]Echo c={connections}:[/]")
                output_path = env_dir / f"echo_c{connections}.json"
                ok = run_oha(echo, args.port, args.duration, connections, output_path)
                if ok:
                    console.print(f"  [green]Done:[/] {output_path}")
                else:
                    console.print(f"  [yellow]Skipped[/]")
        except Exception:
            print_container_logs(env.name)
            raise
        finally:
            stop_container(env.name)

    if not args.no_report:
        generate_report(run_dir, meta, sweep_scenarios)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    args = parse_args()
    check_prerequisites()

    if args.compare:
        run_compare(args)
    elif args.profile:
        run_profile(args)
    elif args.sweep:
        run_sweep(args)
    else:
        run_default(args)


if __name__ == "__main__":
    main()
