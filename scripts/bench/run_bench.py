# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13", "httpx>=0.27"]
# ///
"""APX vs Uvicorn benchmark orchestrator.

Usage:
    uv run scripts/bench/run_bench.py [OPTIONS]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import httpx
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn

console = Console()

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DIR = Path(__file__).resolve().parent
DOCKER_DIR = BENCH_DIR / "docker"
DEFAULT_SCENARIOS = BENCH_DIR / "scenarios.json"
DEFAULT_RESULTS = BENCH_DIR / "results"

SERVERS = {
    "uvicorn": {
        "image": "bench-uvicorn",
        "dockerfile": DOCKER_DIR / "Dockerfile.uvicorn",
        "context": str(BENCH_DIR),
    },
    "apx": {
        "image": "bench-apx",
        "dockerfile": DOCKER_DIR / "Dockerfile.apx",
        "context": str(PROJECT_ROOT),
    },
}

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="APX vs Uvicorn benchmark")
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
        choices=["uvicorn", "apx", "both"],
        default="both",
        help="Which server to benchmark",
    )
    p.add_argument("--no-report", action="store_true", help="Skip report generation")
    p.add_argument(
        "--tokio-threads",
        type=int,
        default=None,
        help="Set TOKIO_WORKER_THREADS in APX container (validates H1)",
    )
    p.add_argument(
        "--warmup",
        type=int,
        default=1000,
        help="Number of warmup requests before benchmarking (default: 1000)",
    )
    p.add_argument(
        "--profile",
        action="store_true",
        help="Capture py-spy flamegraph during APX benchmark",
    )
    p.add_argument(
        "--sweep",
        action="store_true",
        help="Run sweep mode: echo scenario across worker/thread/connection matrix",
    )
    p.add_argument(
        "--profile-asgi",
        action="store_true",
        help="Run ASGI profiling: measures Python-level per-request timing for both servers",
    )
    p.add_argument(
        "--profile-asgi-duration",
        default="15s",
        help="Duration for ASGI profiling load (default: 15s)",
    )
    return p.parse_args()


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


def build_image(name: str, dockerfile: Path, context: str) -> None:
    """Build a Docker image."""
    console.print(f"\n[bold blue]Building {name} image...[/]")
    cmd = [
        "docker", "build",
        "-f", str(dockerfile),
        "-t", f"bench-{name}",
        context,
    ]
    env = {**os.environ, "DOCKER_BUILDKIT": "1"}
    result = subprocess.run(cmd, env=env, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to build {name} image.")
        sys.exit(1)
    console.print(f"[green]Built bench-{name}[/]")


def remove_stale_container(name: str) -> None:
    """Remove container if it exists (from a previous failed run)."""
    subprocess.run(
        ["docker", "rm", "-f", f"bench-{name}"],
        capture_output=True, check=False,
    )


def start_container(
    name: str,
    port: int,
    cpus: str,
    memory: str,
    *,
    tokio_threads: int | None = None,
    extra_env: dict[str, str] | None = None,
) -> str:
    """Start a container, return container ID."""
    remove_stale_container(name)
    cmd = [
        "docker", "run", "-d",
        "--name", f"bench-{name}",
        f"--cpus={cpus}",
        f"--memory={memory}",
        "-p", f"{port}:8000",
    ]

    # Pass environment variables to the container.
    env_vars: dict[str, str] = {}
    if tokio_threads is not None and name == "apx":
        env_vars["TOKIO_WORKER_THREADS"] = str(tokio_threads)
    if extra_env:
        env_vars.update(extra_env)
    for k, v in env_vars.items():
        cmd.extend(["-e", f"{k}={v}"])

    # py-spy needs SYS_PTRACE capability.
    if extra_env and extra_env.get("_PROFILE"):
        cmd.extend(["--cap-add", "SYS_PTRACE"])

    cmd.append(f"bench-{name}")
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to start {name}: {result.stderr}")
        sys.exit(1)
    container_id = result.stdout.strip()
    env_str = f" env={env_vars}" if env_vars else ""
    console.print(f"[green]Started bench-{name}[/] ({container_id[:12]}){env_str}")
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
# Health check
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


# ---------------------------------------------------------------------------
# Warmup
# ---------------------------------------------------------------------------


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
# py-spy profiling
# ---------------------------------------------------------------------------


def run_profile(name: str, port: int, duration: str, results_dir: Path) -> None:
    """Capture a py-spy flamegraph during a benchmark run."""
    console.print(f"\n  [bold yellow]Profiling {name}...[/]")

    # Warmup before profiling.
    run_warmup(port, 2000)

    # Parse duration to seconds for py-spy.
    dur_s = _parse_duration_secs(duration)

    # Start py-spy in the container (background).
    spy_cmd = [
        "docker", "exec", "-d", f"bench-{name}",
        "py-spy", "record",
        "-d", str(dur_s),
        "-o", "/tmp/profile.svg",
        "--pid", "1",
    ]
    subprocess.run(spy_cmd, check=False)

    # Run oha concurrently for the same duration.
    oha_cmd = [
        "oha", "--no-tui",
        "-z", duration,
        "-c", "100",
        f"http://127.0.0.1:{port}/api/echo",
    ]
    subprocess.run(oha_cmd, capture_output=True, check=False)

    # Wait a moment for py-spy to finish writing.
    time.sleep(2)

    # Copy flamegraph out.
    svg_path = results_dir / "profile.svg"
    cp_cmd = [
        "docker", "cp",
        f"bench-{name}:/tmp/profile.svg",
        str(svg_path),
    ]
    result = subprocess.run(cp_cmd, capture_output=True, text=True, check=False)
    if result.returncode == 0:
        console.print(f"  [green]Flamegraph saved:[/] {svg_path}")
    else:
        console.print(f"  [yellow]Warning:[/] Failed to copy flamegraph: {result.stderr[:200]}")


def _parse_duration_secs(duration: str) -> int:
    """Parse oha-style duration string (e.g. '10s', '1m') to seconds."""
    d = duration.strip()
    if d.endswith("s"):
        return int(d[:-1])
    if d.endswith("m"):
        return int(d[:-1]) * 60
    return int(d)


# ---------------------------------------------------------------------------
# oha runner
# ---------------------------------------------------------------------------


def run_oha(
    scenario: dict,
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
        "-m", scenario["method"],
    ]

    if "body" in scenario:
        cmd.extend(["-d", json.dumps(scenario["body"])])
        cmd.extend(["-T", "application/json"])

    cmd.append(f"http://127.0.0.1:{port}{scenario['path']}")

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(
            f"  [yellow]Warning:[/] oha failed for {scenario['name']}: {result.stderr[:200]}"
        )
        return False

    output_path.write_text(result.stdout)
    return True


# ---------------------------------------------------------------------------
# Sweep mode
# ---------------------------------------------------------------------------

SWEEP_WORKERS = [1, 2]
SWEEP_TOKIO_THREADS = [1, 2, 4]
SWEEP_CONNECTIONS = [10, 50, 100, 200]


def run_sweep(args: argparse.Namespace) -> None:
    """Run echo scenario across a worker/thread/connection matrix."""
    echo_scenario = {"name": "echo", "method": "GET", "path": "/api/echo"}

    servers = ["uvicorn", "apx"] if args.server == "both" else [args.server]

    if not args.skip_build:
        for server in servers:
            cfg = SERVERS[server]
            build_image(server, cfg["dockerfile"], cfg["context"])

    sweep_dir = args.results_dir / "sweep"
    sweep_dir.mkdir(parents=True, exist_ok=True)

    results: list[dict] = []

    for server in servers:
        thread_values = SWEEP_TOKIO_THREADS if server == "apx" else [None]
        for workers in SWEEP_WORKERS:
            for tokio_threads in thread_values:
                for connections in SWEEP_CONNECTIONS:
                    label = f"{server}_w{workers}"
                    if tokio_threads is not None:
                        label += f"_t{tokio_threads}"
                    label += f"_c{connections}"

                    console.print(f"\n[cyan]Sweep:[/] {label}")

                    # Build custom CMD for different worker counts.
                    # We override the container CMD via docker run args.
                    container_id = _start_sweep_container(
                        server, args.port, args.cpus, args.memory,
                        workers=workers, tokio_threads=tokio_threads,
                    )
                    try:
                        wait_for_health(args.port)
                        run_warmup(args.port, args.warmup)

                        output_path = sweep_dir / f"{label}.json"
                        ok = run_oha(
                            echo_scenario, args.port, args.duration,
                            connections, output_path,
                        )
                        if ok:
                            results.append({
                                "server": server,
                                "workers": workers,
                                "tokio_threads": tokio_threads,
                                "connections": connections,
                                "file": str(output_path.name),
                            })
                            console.print(f"  [green]Done:[/] {output_path}")
                        else:
                            console.print(f"  [yellow]Skipped:[/] {label}")
                    except Exception:
                        print_container_logs(server)
                        raise
                    finally:
                        stop_container(server)

    # Write sweep manifest for report.py.
    manifest_path = sweep_dir / "manifest.json"
    manifest_path.write_text(json.dumps(results, indent=2))
    console.print(f"\n[bold green]Sweep complete.[/] {len(results)} configurations tested.")
    console.print(f"Manifest: {manifest_path}")

    if not args.no_report:
        console.print("\n[bold blue]Generating sweep report...[/]")
        subprocess.run(
            [
                "uv", "run", str(BENCH_DIR / "report.py"),
                "--results-dir", str(args.results_dir),
                "--sweep",
            ],
            check=False,
        )


def _start_sweep_container(
    name: str,
    port: int,
    cpus: str,
    memory: str,
    *,
    workers: int,
    tokio_threads: int | None,
) -> str:
    """Start a container with overridden worker count for sweep."""
    remove_stale_container(name)
    cmd = [
        "docker", "run", "-d",
        "--name", f"bench-{name}",
        f"--cpus={cpus}",
        f"--memory={memory}",
        "-p", f"{port}:8000",
    ]

    env_vars: dict[str, str] = {}
    if tokio_threads is not None and name == "apx":
        env_vars["TOKIO_WORKER_THREADS"] = str(tokio_threads)
    for k, v in env_vars.items():
        cmd.extend(["-e", f"{k}={v}"])

    cmd.append(f"bench-{name}")

    # Override CMD to set worker count.
    if name == "apx":
        cmd.extend([
            "apx", "serve", "manifest.json",
            "--host", "0.0.0.0", "--port", "8000",
            "--workers", str(workers),
        ])
    elif name == "uvicorn":
        cmd.extend([
            "uvicorn", "app.main:app",
            "--host", "0.0.0.0", "--port", "8000",
            "--workers", str(workers),
            "--loop", "uvloop",
            "--http", "httptools",
        ])

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to start {name}: {result.stderr}")
        sys.exit(1)
    container_id = result.stdout.strip()
    thread_str = f" tokio_threads={tokio_threads}" if tokio_threads else ""
    console.print(
        f"[green]Started bench-{name}[/] ({container_id[:12]})"
        f" workers={workers}{thread_str}"
    )
    return container_id


# ---------------------------------------------------------------------------
# ASGI profiling mode
# ---------------------------------------------------------------------------

PROFILE_SCENARIOS = [
    {"name": "echo", "method": "GET", "path": "/api/echo"},
    {"name": "health", "method": "GET", "path": "/api/health"},
    {"name": "get_item", "method": "GET", "path": "/api/items/1"},
    {"name": "list_items", "method": "GET", "path": "/api/items"},
    {"name": "create_item", "method": "POST", "path": "/api/items",
     "body": {"name": "bench-item", "price": 9.99, "tags": ["test"]}},
]


def run_profile_asgi(args: argparse.Namespace) -> None:
    """Run ASGI-level profiling for both servers and analyze."""
    servers = ["uvicorn", "apx"] if args.server == "both" else [args.server]

    if not args.skip_build:
        for server in servers:
            cfg = SERVERS[server]
            build_image(server, cfg["dockerfile"], cfg["context"])

    profile_dir = args.results_dir / "profile"
    profile_dir.mkdir(parents=True, exist_ok=True)

    for server in servers:
        console.print(f"\n[bold magenta]{'=' * 50}[/]")
        console.print(f"[bold magenta]ASGI Profiling: {server}[/]")
        console.print(f"[bold magenta]{'=' * 50}[/]")

        container_id = start_container(
            server, args.port, args.cpus, args.memory,
            tokio_threads=args.tokio_threads if server == "apx" else None,
            extra_env={"APX_BENCH_PROFILE": "1"},
        )
        try:
            wait_for_health(args.port)
            run_warmup(args.port, args.warmup)

            # Run each scenario under load.
            for scenario in PROFILE_SCENARIOS:
                name = scenario["name"]
                console.print(f"  [cyan]Profiling:[/] {name}")
                run_oha(
                    scenario, args.port, args.profile_asgi_duration,
                    args.connections, profile_dir / f"_oha_{server}_{name}.json",
                )

            # Copy profiling JSONL out of container.
            jsonl_path = profile_dir / f"{server}.jsonl"
            cp_cmd = [
                "docker", "cp",
                f"bench-{server}:/tmp/bench_profile.jsonl",
                str(jsonl_path),
            ]
            result = subprocess.run(cp_cmd, capture_output=True, text=True, check=False)
            if result.returncode == 0:
                console.print(f"  [green]Profile data:[/] {jsonl_path}")
            else:
                console.print(f"  [red]Error copying profile:[/] {result.stderr[:200]}")
        except Exception:
            print_container_logs(server)
            raise
        finally:
            stop_container(server)

    # Run analysis.
    console.print("\n[bold blue]Analyzing profiling data...[/]")
    analysis_cmd = ["uv", "run", str(BENCH_DIR / "profile_analysis.py")]
    apx_jsonl = profile_dir / "apx.jsonl"
    uvi_jsonl = profile_dir / "uvicorn.jsonl"
    if apx_jsonl.exists():
        analysis_cmd.extend(["--apx", str(apx_jsonl)])
    if uvi_jsonl.exists():
        analysis_cmd.extend(["--uvicorn", str(uvi_jsonl)])
    subprocess.run(analysis_cmd, check=False)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    args = parse_args()
    check_prerequisites()

    # ASGI profiling mode — runs its own flow and exits.
    if args.profile_asgi:
        run_profile_asgi(args)
        return

    # Sweep mode — runs its own matrix and exits.
    if args.sweep:
        run_sweep(args)
        return

    if not args.scenarios.exists():
        console.print(f"[red]Error:[/] Scenarios file not found: {args.scenarios}")
        sys.exit(1)
    scenarios = json.loads(args.scenarios.read_text())

    servers = ["uvicorn", "apx"] if args.server == "both" else [args.server]

    if not args.skip_build:
        for server in servers:
            cfg = SERVERS[server]
            build_image(server, cfg["dockerfile"], cfg["context"])

    for server in servers:
        console.print(f"\n[bold magenta]{'=' * 50}[/]")
        console.print(f"[bold magenta]Benchmarking: {server}[/]")
        console.print(f"[bold magenta]{'=' * 50}[/]")

        results_dir = args.results_dir / server
        results_dir.mkdir(parents=True, exist_ok=True)

        extra_env: dict[str, str] = {}
        if args.profile and server == "apx":
            extra_env["_PROFILE"] = "1"

        container_id = start_container(
            server, args.port, args.cpus, args.memory,
            tokio_threads=args.tokio_threads if server == "apx" else None,
            extra_env=extra_env or None,
        )
        try:
            wait_for_health(args.port)
            run_warmup(args.port, args.warmup)

            # Profile if requested (APX only).
            if args.profile and server == "apx":
                run_profile(server, args.port, args.duration, results_dir)

            for scenario in scenarios:
                name = scenario["name"]
                console.print(f"\n  [cyan]Running:[/] {name} ({scenario['method']} {scenario['path']})")
                output_path = results_dir / f"{name}.json"
                ok = run_oha(scenario, args.port, args.duration, args.connections, output_path)
                if ok:
                    console.print(f"  [green]Done:[/] {output_path}")
                else:
                    console.print(f"  [yellow]Skipped:[/] {name}")
        except Exception:
            print_container_logs(server)
            raise
        finally:
            stop_container(server)

    if not args.no_report:
        console.print("\n[bold blue]Generating report...[/]")
        subprocess.run(
            ["uv", "run", str(BENCH_DIR / "report.py"), "--results-dir", str(args.results_dir)],
            check=False,
        )


if __name__ == "__main__":
    main()
