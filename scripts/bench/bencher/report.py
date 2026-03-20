"""Report generation from DB records.

Produces the same JSON schema as the original filesystem-based generate_report()
in main.py, but works from in-memory ScenarioResult / ProfileResult lists.
"""

from __future__ import annotations

import json
import logging
import sys
from pathlib import Path

from .models import BenchConfig, ProfileResult, ScenarioResult

logger = logging.getLogger("bencher.report")


def _safe_ratio(a: float, b: float) -> float | None:
    return round(a / b, 4) if b else None


def _parse_oha_result(raw: dict, env_name: str, scenario_name: str) -> dict | None:
    """Parse raw oha JSON into throughput/latency summary. Returns None on failure."""
    summary = raw.get("summary", {})
    percentiles = raw.get("latencyPercentiles", {})

    def _f(d: dict, key: str) -> float:
        v = d.get(key)
        return float(v) if v is not None else 0.0

    success_rate = _f(summary, "successRate")
    if success_rate == 0.0:
        return None

    return {
        "environment": env_name,
        "scenario": scenario_name,
        "requests_per_sec": _f(summary, "requestsPerSec"),
        "latency_p50_ms": _f(percentiles, "p50") * 1000,
        "latency_p90_ms": _f(percentiles, "p90") * 1000,
        "latency_p99_ms": _f(percentiles, "p99") * 1000,
        "success_rate": success_rate,
    }


def generate_report(
    config: BenchConfig,
    scenario_results: list[ScenarioResult],
    profile_results: list[ProfileResult],
    meta: dict | None = None,
) -> dict:
    """Build report dict from DB records. Same schema as current report.json."""
    # Lazy import — profile_analysis lives in the parent bench directory or
    # is copied alongside during assembly.
    bench_dir = Path(__file__).resolve().parent.parent
    if str(bench_dir) not in sys.path:
        sys.path.insert(0, str(bench_dir))
    from profile_analysis import analyze_records  # noqa: E402

    logger.info(
        "Generating report: name=%s, %d scenario results, %d profile results",
        config.name,
        len(scenario_results),
        len(profile_results),
    )
    env_names = sorted(config.environments.keys())

    # Build scenario_meta from config.scenarios (or default to what we got).
    scenario_meta: dict[str, dict] = {}
    if config.scenarios:
        for s in config.scenarios:
            scenario_meta[s.name] = {"method": s.method, "path": s.path}

    # ---- Organise oha results by env × scenario ----
    all_oha: dict[str, dict[str, dict]] = {e: {} for e in env_names}
    all_scenario_names: set[str] = set()

    for sr in scenario_results:
        raw = json.loads(sr.raw_oha_json)
        all_oha[sr.environment][sr.scenario] = raw
        all_scenario_names.add(sr.scenario)

    scenario_names = sorted(all_scenario_names)

    # ---- Build scenarios section ----
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
            parsed = _parse_oha_result(raw, env_name, sname)
            if parsed:
                throughput_rps[env_name] = round(parsed["requests_per_sec"], 1)
                latency_ms[env_name] = {
                    "p50": round(parsed["latency_p50_ms"], 2),
                    "p90": round(parsed["latency_p90_ms"], 2),
                    "p99": round(parsed["latency_p99_ms"], 2),
                }

        # Pairwise ratios.
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

    # ---- Load profiling data ----
    profiling_section: dict[str, dict] = {}
    for pr in profile_results:
        # Write JSONL to a temp in-memory approach: load_records expects a path,
        # so we parse directly.
        info = None
        reqs: list[dict] = []
        for line in pr.raw_jsonl.splitlines():
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            if rec.get("type") == "info":
                info = rec
            elif rec.get("type") == "req":
                reqs.append(rec)

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

        profiling_section[pr.environment] = {
            "info": {
                "loop": info.get("loop", "?") if info else "?",
                "python": info.get("python", "?") if info else "?",
                "pid": info.get("pid", "?") if info else "?",
            },
            "endpoints": endpoints,
        }

    # ---- Compute profiling ratios ----
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

    # ---- Compute summary ----
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
                pr_data = profiling_ratios.get(path, {})
                h = pr_data.get(f"handler_p50_{label}")
                s = pr_data.get(f"send_p50_{label}")
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

    # ---- Assemble ----
    logger.info(
        "Report built: %d scenarios, %d profiled envs",
        len(scenarios_section),
        len(profiling_section),
    )

    report = {
        "meta": meta
        or {
            "name": config.name,
            "duration": config.duration,
            "connections": config.connections,
            "warmup_requests": config.warmup_requests,
            "mode": "bench+profile" if config.profile else "bench",
            "environments": {k: v for k, v in config.environments.items()},
        },
        "scenarios": scenarios_section,
        "profiling": profiling_section,
        "profiling_ratios": profiling_ratios,
        "summary": summary,
    }

    return report
