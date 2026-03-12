# /// script
# requires-python = ">=3.11"
# dependencies = ["rich>=13"]
# ///
"""Analyze ASGI profiling JSONL from APX and Uvicorn containers.

Usage:
    uv run scripts/bench/profile_analysis.py --apx results/profile/apx.jsonl --uvicorn results/profile/uvicorn.jsonl
    uv run scripts/bench/profile_analysis.py --apx results/profile/apx.jsonl  # APX only
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from rich.console import Console
from rich.table import Table

console = Console()


def load_records(path: Path) -> tuple[dict | None, list[dict]]:
    """Load JSONL, return (info_record, request_records)."""
    info = None
    reqs: list[dict] = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            if rec.get("type") == "info":
                info = rec
            elif rec.get("type") == "req":
                reqs.append(rec)
    return info, reqs


def percentile(values: list[float], p: float) -> float:
    """Compute p-th percentile (0-100)."""
    if not values:
        return 0.0
    values = sorted(values)
    k = (len(values) - 1) * p / 100.0
    f = int(k)
    c = f + 1
    if c >= len(values):
        return values[f]
    return values[f] + (k - f) * (values[c] - values[f])


def ns_to_us(ns: float) -> float:
    return ns / 1000.0


def analyze_records(reqs: list[dict], path_filter: str | None = None) -> dict[str, dict]:
    """Group by path, compute stats per field."""
    by_path: dict[str, list[dict]] = {}
    for r in reqs:
        p = r["path"]
        if path_filter and path_filter not in p:
            continue
        by_path.setdefault(p, []).append(r)

    stats: dict[str, dict] = {}
    fields = ["total_ns", "handler_ns", "recv_ns", "send_ns"]
    for path, records in sorted(by_path.items()):
        s: dict = {"count": len(records)}
        for field in fields:
            vals = [r[field] for r in records]
            label = field.replace("_ns", "")
            s[f"{label}_p50_us"] = ns_to_us(percentile(vals, 50))
            s[f"{label}_p99_us"] = ns_to_us(percentile(vals, 99))
            s[f"{label}_avg_us"] = ns_to_us(sum(vals) / len(vals))
        # Derive receive/send call counts.
        s["recv_calls_avg"] = sum(r["recv_n"] for r in records) / len(records)
        s["send_calls_avg"] = sum(r["send_n"] for r in records) / len(records)
        stats[path] = s
    return stats


def print_stats(name: str, info: dict | None, stats: dict[str, dict]) -> None:
    """Print a breakdown table for one server."""
    console.print(f"\n[bold cyan]{name}[/]")
    if info:
        console.print(f"  loop: {info.get('loop', '?')}  python: {info.get('python', '?')}  pid: {info.get('pid', '?')}")

    table = Table(show_header=True, header_style="bold")
    table.add_column("Path", style="dim")
    table.add_column("N", justify="right")
    table.add_column("total p50", justify="right")
    table.add_column("total p99", justify="right")
    table.add_column("handler p50", justify="right")
    table.add_column("handler p99", justify="right")
    table.add_column("recv p50", justify="right")
    table.add_column("send p50", justify="right")
    table.add_column("recv/send calls", justify="right")

    for path, s in stats.items():
        table.add_row(
            path,
            str(s["count"]),
            f"{s['total_p50_us']:.0f}µs",
            f"{s['total_p99_us']:.0f}µs",
            f"{s['handler_p50_us']:.0f}µs",
            f"{s['handler_p99_us']:.0f}µs",
            f"{s['recv_p50_us']:.0f}µs",
            f"{s['send_p50_us']:.0f}µs",
            f"{s['recv_calls_avg']:.1f}/{s['send_calls_avg']:.1f}",
        )

    console.print(table)


def print_comparison(apx_stats: dict[str, dict], uvi_stats: dict[str, dict]) -> None:
    """Side-by-side comparison of matching paths."""
    console.print("\n[bold magenta]Comparison: APX vs Uvicorn (µs, p50)[/]")

    table = Table(show_header=True, header_style="bold")
    table.add_column("Path", style="dim")
    table.add_column("APX total", justify="right")
    table.add_column("Uvi total", justify="right")
    table.add_column("Ratio", justify="right")
    table.add_column("APX handler", justify="right")
    table.add_column("Uvi handler", justify="right")
    table.add_column("H.Ratio", justify="right")
    table.add_column("APX send", justify="right")
    table.add_column("Uvi send", justify="right")

    common_paths = sorted(set(apx_stats.keys()) & set(uvi_stats.keys()))
    for path in common_paths:
        a = apx_stats[path]
        u = uvi_stats[path]
        total_ratio = a["total_p50_us"] / u["total_p50_us"] if u["total_p50_us"] > 0 else 0
        handler_ratio = a["handler_p50_us"] / u["handler_p50_us"] if u["handler_p50_us"] > 0 else 0

        ratio_style = "red" if total_ratio > 1.2 else "green" if total_ratio < 0.9 else ""
        h_ratio_style = "red" if handler_ratio > 1.2 else "green" if handler_ratio < 0.9 else ""

        table.add_row(
            path,
            f"{a['total_p50_us']:.0f}",
            f"{u['total_p50_us']:.0f}",
            f"[{ratio_style}]{total_ratio:.2f}x[/]",
            f"{a['handler_p50_us']:.0f}",
            f"{u['handler_p50_us']:.0f}",
            f"[{h_ratio_style}]{handler_ratio:.2f}x[/]",
            f"{a['send_p50_us']:.0f}",
            f"{u['send_p50_us']:.0f}",
        )

    console.print(table)

    # Summary.
    if common_paths:
        apx_avg_handler = sum(apx_stats[p]["handler_p50_us"] for p in common_paths) / len(common_paths)
        uvi_avg_handler = sum(uvi_stats[p]["handler_p50_us"] for p in common_paths) / len(common_paths)
        apx_avg_send = sum(apx_stats[p]["send_p50_us"] for p in common_paths) / len(common_paths)
        uvi_avg_send = sum(uvi_stats[p]["send_p50_us"] for p in common_paths) / len(common_paths)
        console.print(f"\n  Avg handler p50: APX {apx_avg_handler:.0f}µs vs Uvicorn {uvi_avg_handler:.0f}µs"
                      f" ({apx_avg_handler/uvi_avg_handler:.2f}x)" if uvi_avg_handler else "")
        console.print(f"  Avg send p50:    APX {apx_avg_send:.0f}µs vs Uvicorn {uvi_avg_send:.0f}µs"
                      f" ({apx_avg_send/uvi_avg_send:.2f}x)" if uvi_avg_send else "")


def main():
    p = argparse.ArgumentParser(description="Analyze ASGI profiling data")
    p.add_argument("--apx", type=Path, help="APX profiling JSONL")
    p.add_argument("--uvicorn", type=Path, help="Uvicorn profiling JSONL")
    p.add_argument("--path", type=str, default=None, help="Filter to paths containing this string")
    args = p.parse_args()

    if not args.apx and not args.uvicorn:
        p.error("Provide at least one of --apx or --uvicorn")

    apx_stats = None
    uvi_stats = None

    if args.apx:
        if not args.apx.exists():
            console.print(f"[red]Error:[/] {args.apx} not found")
            sys.exit(1)
        info, reqs = load_records(args.apx)
        apx_stats = analyze_records(reqs, args.path)
        print_stats("APX", info, apx_stats)

    if args.uvicorn:
        if not args.uvicorn.exists():
            console.print(f"[red]Error:[/] {args.uvicorn} not found")
            sys.exit(1)
        info, reqs = load_records(args.uvicorn)
        uvi_stats = analyze_records(reqs, args.path)
        print_stats("Uvicorn", info, uvi_stats)

    if apx_stats and uvi_stats:
        print_comparison(apx_stats, uvi_stats)


if __name__ == "__main__":
    main()
