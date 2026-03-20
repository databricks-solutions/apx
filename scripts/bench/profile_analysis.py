"""Profiling analysis library for ASGI benchmark data.

Provides functions to load and analyze ASGI profiling JSONL produced by
the bench profiling middleware (app/profiling.py).
"""

from __future__ import annotations

import json
from pathlib import Path


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


def analyze_records(
    reqs: list[dict], path_filter: str | None = None
) -> dict[str, dict]:
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
        s["recv_calls_avg"] = sum(r["recv_n"] for r in records) / len(records)
        s["send_calls_avg"] = sum(r["send_n"] for r in records) / len(records)
        stats[path] = s
    return stats
