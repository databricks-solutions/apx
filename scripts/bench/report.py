# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13"]
# ///
"""Parse oha JSON results and generate APX vs Uvicorn comparison.

Usage:
    uv run scripts/bench/report.py [OPTIONS]
"""
from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path

from rich.console import Console
from rich.table import Table

console = Console()

DEFAULT_RESULTS = Path(__file__).resolve().parent / "results"

# ---------------------------------------------------------------------------
# Data model
# ---------------------------------------------------------------------------


@dataclass
class ScenarioResult:
    name: str
    requests_per_sec: float
    latency_p50_ms: float
    latency_p90_ms: float
    latency_p99_ms: float
    success_rate: float
    total_requests: int
    transfer_per_sec_kb: float


def parse_oha_json(path: Path, name: str) -> ScenarioResult | None:
    """Parse an oha JSON output file into a ScenarioResult."""
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, FileNotFoundError):
        return None

    summary = data.get("summary", {})
    percentiles = data.get("latencyPercentiles", {})

    return ScenarioResult(
        name=name,
        requests_per_sec=summary.get("requestsPerSec", 0.0),
        latency_p50_ms=percentiles.get("p50", 0.0) * 1000,
        latency_p90_ms=percentiles.get("p90", 0.0) * 1000,
        latency_p99_ms=percentiles.get("p99", 0.0) * 1000,
        success_rate=summary.get("successRate", 0.0),
        total_requests=summary.get("total", 0),
        transfer_per_sec_kb=summary.get("sizePerSec", 0.0) / 1024,
    )


# ---------------------------------------------------------------------------
# Data loading
# ---------------------------------------------------------------------------


def load_results(results_dir: Path) -> dict[str, dict[str, ScenarioResult]]:
    """Load all results: {server: {scenario_name: ScenarioResult}}."""
    results: dict[str, dict[str, ScenarioResult]] = {}

    for server_dir in sorted(results_dir.iterdir()):
        if not server_dir.is_dir():
            continue
        server_name = server_dir.name
        results[server_name] = {}
        for json_file in sorted(server_dir.glob("*.json")):
            scenario_name = json_file.stem
            parsed = parse_oha_json(json_file, scenario_name)
            if parsed:
                results[server_name][scenario_name] = parsed

    return results


# ---------------------------------------------------------------------------
# Terminal report (rich)
# ---------------------------------------------------------------------------


def _ratio_str(apx_val: float, uv_val: float, higher_is_better: bool) -> str:
    """Format a ratio with color. Green = APX wins."""
    if uv_val == 0:
        return "N/A"
    ratio = apx_val / uv_val if higher_is_better else uv_val / apx_val
    color = "green" if ratio > 1.0 else "red" if ratio < 1.0 else "white"
    return f"[{color}]{ratio:.2f}x[/{color}]"


def _winner(apx_val: float, uv_val: float, higher_is_better: bool) -> str:
    if higher_is_better:
        return "[green]APX[/]" if apx_val > uv_val else "[red]Uvicorn[/]"
    return "[green]APX[/]" if apx_val < uv_val else "[red]Uvicorn[/]"


def print_throughput_table(results: dict[str, dict[str, ScenarioResult]]) -> None:
    """Print throughput comparison table."""
    uvicorn = results.get("uvicorn", {})
    apx = results.get("apx", {})
    scenarios = sorted(set(uvicorn.keys()) | set(apx.keys()))

    table = Table(title="Throughput (requests/sec)")
    table.add_column("Scenario", style="cyan")
    table.add_column("Uvicorn", justify="right")
    table.add_column("APX", justify="right")
    table.add_column("Ratio", justify="right")
    table.add_column("Winner", justify="center")

    for name in scenarios:
        uv = uvicorn.get(name)
        ax = apx.get(name)
        if uv and ax:
            table.add_row(
                name,
                f"{uv.requests_per_sec:,.1f}",
                f"{ax.requests_per_sec:,.1f}",
                _ratio_str(ax.requests_per_sec, uv.requests_per_sec, higher_is_better=True),
                _winner(ax.requests_per_sec, uv.requests_per_sec, higher_is_better=True),
            )
        elif uv:
            table.add_row(name, f"{uv.requests_per_sec:,.1f}", "N/A", "N/A", "N/A")
        elif ax:
            table.add_row(name, "N/A", f"{ax.requests_per_sec:,.1f}", "N/A", "N/A")

    console.print(table)


def print_latency_table(results: dict[str, dict[str, ScenarioResult]]) -> None:
    """Print latency comparison table."""
    uvicorn = results.get("uvicorn", {})
    apx = results.get("apx", {})
    scenarios = sorted(set(uvicorn.keys()) | set(apx.keys()))

    table = Table(title="Latency (ms) — p50 / p90 / p99")
    table.add_column("Scenario", style="cyan")
    table.add_column("Uvicorn", justify="right")
    table.add_column("APX", justify="right")
    table.add_column("p99 Ratio", justify="right")

    for name in scenarios:
        uv = uvicorn.get(name)
        ax = apx.get(name)
        if uv and ax:
            table.add_row(
                name,
                f"{uv.latency_p50_ms:.1f} / {uv.latency_p90_ms:.1f} / {uv.latency_p99_ms:.1f}",
                f"{ax.latency_p50_ms:.1f} / {ax.latency_p90_ms:.1f} / {ax.latency_p99_ms:.1f}",
                _ratio_str(ax.latency_p99_ms, uv.latency_p99_ms, higher_is_better=False),
            )
        elif uv:
            table.add_row(
                name,
                f"{uv.latency_p50_ms:.1f} / {uv.latency_p90_ms:.1f} / {uv.latency_p99_ms:.1f}",
                "N/A", "N/A",
            )

    console.print(table)


def print_summary(results: dict[str, dict[str, ScenarioResult]]) -> None:
    """Print average improvement summary."""
    uvicorn = results.get("uvicorn", {})
    apx = results.get("apx", {})
    common = set(uvicorn.keys()) & set(apx.keys())

    if not common:
        console.print("[yellow]No common scenarios to compare.[/]")
        return

    throughput_ratios = []
    latency_ratios = []

    for name in common:
        uv = uvicorn[name]
        ax = apx[name]
        if uv.requests_per_sec > 0:
            throughput_ratios.append(ax.requests_per_sec / uv.requests_per_sec)
        if uv.latency_p99_ms > 0 and ax.latency_p99_ms > 0:
            latency_ratios.append(uv.latency_p99_ms / ax.latency_p99_ms)

    if throughput_ratios:
        avg_tp = sum(throughput_ratios) / len(throughput_ratios)
        console.print(f"\n[bold]Average throughput ratio (APX/Uvicorn):[/] {avg_tp:.2f}x")
    if latency_ratios:
        avg_lat = sum(latency_ratios) / len(latency_ratios)
        console.print(f"[bold]Average p99 latency ratio (Uvicorn/APX):[/] {avg_lat:.2f}x")


# ---------------------------------------------------------------------------
# Markdown report
# ---------------------------------------------------------------------------


def generate_markdown(
    results: dict[str, dict[str, ScenarioResult]],
    output: Path,
) -> None:
    """Write a markdown comparison report."""
    uvicorn = results.get("uvicorn", {})
    apx = results.get("apx", {})
    scenarios = sorted(set(uvicorn.keys()) | set(apx.keys()))

    lines: list[str] = [
        "# APX vs Uvicorn Benchmark Results",
        "",
        "## Throughput (requests/sec)",
        "",
        "| Scenario | Uvicorn | APX | Ratio (APX/UV) |",
        "|----------|--------:|----:|---------------:|",
    ]

    for name in scenarios:
        uv = uvicorn.get(name)
        ax = apx.get(name)
        uv_rps = f"{uv.requests_per_sec:,.1f}" if uv else "N/A"
        ax_rps = f"{ax.requests_per_sec:,.1f}" if ax else "N/A"
        ratio = "N/A"
        if uv and ax and uv.requests_per_sec > 0:
            ratio = f"{ax.requests_per_sec / uv.requests_per_sec:.2f}x"
        lines.append(f"| {name} | {uv_rps} | {ax_rps} | {ratio} |")

    lines.extend([
        "",
        "## Latency (ms)",
        "",
        "| Scenario | Uvicorn p50/p90/p99 | APX p50/p90/p99 |",
        "|----------|--------------------:|----------------:|",
    ])

    for name in scenarios:
        uv = uvicorn.get(name)
        ax = apx.get(name)
        uv_lat = f"{uv.latency_p50_ms:.1f}/{uv.latency_p90_ms:.1f}/{uv.latency_p99_ms:.1f}" if uv else "N/A"
        ax_lat = f"{ax.latency_p50_ms:.1f}/{ax.latency_p90_ms:.1f}/{ax.latency_p99_ms:.1f}" if ax else "N/A"
        lines.append(f"| {name} | {uv_lat} | {ax_lat} |")

    lines.append("")
    output.write_text("\n".join(lines))


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate benchmark comparison report")
    parser.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("-o", "--output", type=Path, default=None)
    parser.add_argument(
        "--format",
        choices=["terminal", "markdown", "both"],
        default="both",
    )
    args = parser.parse_args()

    if not args.results_dir.exists():
        console.print(f"[red]Error:[/] Results directory not found: {args.results_dir}")
        sys.exit(1)

    results = load_results(args.results_dir)

    if not results:
        console.print("[yellow]No results found.[/]")
        sys.exit(1)

    if args.format in ("terminal", "both"):
        console.print("\n[bold]APX vs Uvicorn Benchmark Results[/]\n")
        print_throughput_table(results)
        console.print()
        print_latency_table(results)
        print_summary(results)

    if args.format in ("markdown", "both"):
        md_output = args.output or (args.results_dir / "report.md")
        generate_markdown(results, md_output)
        console.print(f"\n[dim]Markdown report saved to {md_output}[/]")


if __name__ == "__main__":
    main()
