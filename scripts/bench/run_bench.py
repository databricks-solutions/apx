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
    result = subprocess.run(cmd, check=False)
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


def start_container(name: str, port: int, cpus: str, memory: str) -> str:
    """Start a container, return container ID."""
    remove_stale_container(name)
    cmd = [
        "docker", "run", "-d",
        "--name", f"bench-{name}",
        f"--cpus={cpus}",
        f"--memory={memory}",
        "-p", f"{port}:8000",
        f"bench-{name}",
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(f"[red]Error:[/] Failed to start {name}: {result.stderr}")
        sys.exit(1)
    container_id = result.stdout.strip()
    console.print(f"[green]Started bench-{name}[/] ({container_id[:12]})")
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
    url = f"http://localhost:{port}/api/health"
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
            except (httpx.ConnectError, httpx.ReadTimeout):
                pass
            time.sleep(0.5)

    console.print(f"[red]Error:[/] Server did not become healthy within {timeout}s")
    sys.exit(1)


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
        "-j",
        "--no-tui",
        "-z", duration,
        "-c", str(connections),
        "-m", scenario["method"],
    ]

    if "body" in scenario:
        cmd.extend(["-d", json.dumps(scenario["body"])])
        cmd.extend(["-T", "application/json"])

    cmd.append(f"http://localhost:{port}{scenario['path']}")

    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        console.print(
            f"  [yellow]Warning:[/] oha failed for {scenario['name']}: {result.stderr[:200]}"
        )
        return False

    output_path.write_text(result.stdout)
    return True


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    args = parse_args()
    check_prerequisites()

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

        container_id = start_container(server, args.port, args.cpus, args.memory)
        try:
            wait_for_health(args.port)

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
