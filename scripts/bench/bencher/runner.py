"""Benchmark execution logic.

Runs as a BackgroundTask. Uses the Databricks SDK for auth and
app URL resolution. Stores results in Lakebase Autoscaled (PostgreSQL).
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from datetime import datetime, timezone

import httpx
from databricks.sdk import WorkspaceClient
from sqlmodel import Session

from .database import get_engine
from .models import (
    BenchConfig,
    BenchmarkRun,
    ProfileResult,
    RunStatus,
    Scenario,
    ScenarioResult,
)
from .report import generate_report

logger = logging.getLogger("bencher.runner")

# Profiling-specific scenarios (subset).
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
    Scenario(name="yield_once", method="GET", path="/api/yield-once"),
    Scenario(name="cpu_1k", method="GET", path="/api/cpu/1000"),
    Scenario(name="stream_10", method="GET", path="/api/stream/10"),
]

# Default scenarios loaded from scenarios.json at startup (populated in lifespan).
_default_scenarios: list[Scenario] = []

# Cancellation flags: run_id → bool
_cancel_flags: dict[str, bool] = {}


def set_default_scenarios(scenarios: list[Scenario]) -> None:
    global _default_scenarios
    _default_scenarios = scenarios


def request_cancel(run_id: str) -> bool:
    """Signal cancellation for a run. Returns True if the run was trackable."""
    if run_id in _cancel_flags:
        _cancel_flags[run_id] = True
        return True
    return False


def _is_cancelled(run_id: str) -> bool:
    return _cancel_flags.get(run_id, False)


# ---------------------------------------------------------------------------
# Databricks auth helpers
# ---------------------------------------------------------------------------


def _get_token(ws: WorkspaceClient) -> str:
    headers = ws.config.authenticate()
    auth = headers.get("Authorization", "")
    if not auth.startswith("Bearer "):
        raise RuntimeError("Failed to get Databricks token")
    return auth.removeprefix("Bearer ")


def _get_app_url(ws: WorkspaceClient, app_name: str) -> str:
    app_info = ws.apps.get(app_name)
    url = app_info.url or ""
    if not url:
        raise RuntimeError(f"No URL found for {app_name}")
    return url.rstrip("/")


# ---------------------------------------------------------------------------
# Health / warmup / oha
# ---------------------------------------------------------------------------


async def _wait_for_health(url: str, token: str, timeout: float = 120.0) -> None:
    health_url = f"{url}/api/health"
    headers = {"Authorization": f"Bearer {token}"}
    deadline = time.monotonic() + timeout

    logger.info("Waiting for health: %s", health_url)
    async with httpx.AsyncClient() as client:
        while time.monotonic() < deadline:
            try:
                resp = await client.get(health_url, headers=headers, timeout=10.0)
                if resp.status_code == 200:
                    logger.info("Health OK: %s", url)
                    return
            except httpx.HTTPError:
                pass
            await asyncio.sleep(5)

    raise RuntimeError(f"{url} did not become healthy within {timeout}s")


async def _run_warmup(
    oha_path: str, url: str, token: str, warmup_requests: int
) -> None:
    if warmup_requests <= 0:
        return
    logger.info("Warming up %s with %d requests", url, warmup_requests)
    cmd = [
        oha_path,
        "--no-tui",
        "-n",
        str(warmup_requests),
        "-c",
        str(min(warmup_requests, 50)),
        "-H",
        f"Authorization: Bearer {token}",
        f"{url}/api/health",
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL
    )
    await proc.wait()
    logger.info("Warmup done for %s", url)


async def _run_oha(
    oha_path: str,
    scenario: Scenario,
    url: str,
    token: str,
    duration: str,
    connections: int,
) -> str | None:
    """Run oha and return raw JSON output string, or None on failure."""
    target_url = f"{url}{scenario.path}"
    logger.info(
        "Running oha: %s %s (duration=%s, connections=%d)",
        scenario.method,
        target_url,
        duration,
        connections,
    )
    t0 = time.monotonic()

    cmd = [
        oha_path,
        "--output-format",
        "json",
        "--no-tui",
        "-z",
        duration,
        "-c",
        str(connections),
        "-m",
        scenario.method,
        "-H",
        f"Authorization: Bearer {token}",
    ]

    if scenario.body is not None:
        cmd.extend(["-d", json.dumps(scenario.body)])
        cmd.extend(["-T", "application/json"])

    cmd.append(target_url)

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await proc.communicate()
    elapsed = time.monotonic() - t0

    if proc.returncode != 0:
        logger.warning(
            "oha failed for %s (exit=%d, %.1fs): %s",
            scenario.name,
            proc.returncode,
            elapsed,
            stderr[:200],
        )
        return None

    logger.info("oha done: %s (%.1fs)", scenario.name, elapsed)
    return stdout.decode()


async def _extract_profiling(url: str, token: str) -> str | None:
    """Download profiling JSONL text from a benchmark app."""
    logger.info("Extracting profiling data from %s", url)
    headers = {"Authorization": f"Bearer {token}"}
    async with httpx.AsyncClient() as client:
        try:
            resp = await client.get(
                f"{url}/api/profile/dump", headers=headers, timeout=30.0
            )
            if resp.status_code == 200:
                lines = resp.text.strip().count("\n") + 1 if resp.text.strip() else 0
                logger.info("Got %d profiling lines from %s", lines, url)
                return resp.text
        except httpx.HTTPError as exc:
            logger.warning("Error fetching profile from %s: %s", url, exc)
    return None


async def _reset_items(url: str, token: str) -> None:
    """Reset items to defaults before a benchmark pass."""
    logger.info("Resetting items on %s", url)
    headers = {"Authorization": f"Bearer {token}"}
    async with httpx.AsyncClient() as client:
        try:
            resp = await client.post(
                f"{url}/api/items/reset", headers=headers, timeout=10.0
            )
            logger.info("Items reset: %s", resp.text[:100])
        except httpx.HTTPError as exc:
            logger.warning("Failed to reset items on %s: %s", url, exc)


async def _reset_profiling(url: str, token: str) -> None:
    logger.info("Resetting profiling on %s", url)
    headers = {"Authorization": f"Bearer {token}"}
    async with httpx.AsyncClient() as client:
        try:
            await client.delete(
                f"{url}/api/profile/reset", headers=headers, timeout=10.0
            )
        except httpx.HTTPError:
            pass


async def _collect_scheduler_stats(url: str, token: str) -> dict | None:
    """Collect scheduler counters from a benchmark app."""
    logger.info("Collecting scheduler stats from %s", url)
    headers = {"Authorization": f"Bearer {token}"}
    async with httpx.AsyncClient() as client:
        try:
            resp = await client.get(
                f"{url}/api/_bench/scheduler-stats", headers=headers, timeout=10.0
            )
            if resp.status_code == 200:
                return resp.json()
        except httpx.HTTPError as exc:
            logger.warning("Error fetching scheduler stats from %s: %s", url, exc)
    return None


# ---------------------------------------------------------------------------
# Progress helpers
# ---------------------------------------------------------------------------


def _update_run(run_id: str, **kwargs) -> None:
    """Update a BenchmarkRun row."""
    with Session(get_engine()) as session:
        run = session.get(BenchmarkRun, run_id)
        if not run:
            return
        for k, v in kwargs.items():
            setattr(run, k, v)
        run.updated_at = datetime.now(timezone.utc)
        session.add(run)
        session.commit()


def _set_progress(run_id: str, total: int, completed: int, current: str) -> None:
    logger.info("[%s] progress %d/%d — %s", run_id[:8], completed, total, current)
    _update_run(
        run_id,
        progress_json=json.dumps(
            {"total": total, "completed": completed, "current": current}
        ),
    )


# ---------------------------------------------------------------------------
# Main execution
# ---------------------------------------------------------------------------


async def execute_run(run_id: str, config: BenchConfig, oha_path: str) -> None:
    """Execute a full benchmark run. Scheduled via BackgroundTasks."""
    _cancel_flags[run_id] = False
    run_start = time.monotonic()

    logger.info(
        "[%s] Starting run: name=%s, envs=%s, duration=%s, connections=%d, profile=%s",
        run_id[:8],
        config.name,
        list(config.environments.keys()),
        config.duration,
        config.connections,
        config.profile,
    )

    try:
        _update_run(run_id, status=RunStatus.RUNNING)

        ws = WorkspaceClient()
        logger.info("[%s] WorkspaceClient initialized", run_id[:8])

        # Resolve app URLs.
        env_urls: dict[str, str] = {}
        for env_name, app_name in config.environments.items():
            url = _get_app_url(ws, app_name)
            env_urls[env_name] = url
            logger.info("[%s] Resolved %s → %s", run_id[:8], app_name, url)

        scenarios = config.scenarios or _default_scenarios
        mode = "bench+profile" if config.profile else "bench"
        _update_run(run_id, mode=mode)

        env_names = sorted(config.environments.keys())

        # Total steps: (envs × scenarios) + (envs × profile_scenarios if profiling)
        total_bench = len(env_names) * len(scenarios)
        total_profile = len(env_names) * len(PROFILE_SCENARIOS) if config.profile else 0
        total = total_bench + total_profile
        completed = 0
        logger.info(
            "[%s] Total steps: %d (bench=%d, profile=%d)",
            run_id[:8],
            total,
            total_bench,
            total_profile,
        )

        # ---- Health check + warmup ----
        token = _get_token(ws)
        _set_progress(run_id, total, 0, "health check")

        for env_name, url in env_urls.items():
            if _is_cancelled(run_id):
                raise RuntimeError("Run cancelled")
            await _wait_for_health(url, token)

        _set_progress(run_id, total, 0, "warmup")
        for env_name, url in env_urls.items():
            if _is_cancelled(run_id):
                raise RuntimeError("Run cancelled")
            await _run_warmup(oha_path, url, token, config.warmup_requests)

        # ---- Benchmark pass ----
        for env_name in env_names:
            url = env_urls[env_name]
            logger.info("[%s] === Benchmark pass: %s ===", run_id[:8], env_name)
            # Refresh token before each environment pass.
            token = _get_token(ws)
            await _reset_items(url, token)

            for scenario in scenarios:
                if _is_cancelled(run_id):
                    raise RuntimeError("Run cancelled")

                _set_progress(
                    run_id, total, completed, f"bench {env_name}/{scenario.name}"
                )

                raw_json = await _run_oha(
                    oha_path, scenario, url, token, config.duration, config.connections
                )

                if raw_json:
                    raw = json.loads(raw_json)
                    summary = raw.get("summary", {})
                    percentiles = raw.get("latencyPercentiles", {})

                    def _f(d: dict, key: str) -> float:
                        v = d.get(key)
                        return float(v) if v is not None else 0.0

                    success_rate = _f(summary, "successRate")
                    if success_rate > 0:
                        rps = _f(summary, "requestsPerSec")
                        sr = ScenarioResult(
                            run_id=run_id,
                            environment=env_name,
                            scenario=scenario.name,
                            raw_oha_json=raw_json,
                            requests_per_sec=rps,
                            latency_p50_ms=_f(percentiles, "p50") * 1000,
                            latency_p90_ms=_f(percentiles, "p90") * 1000,
                            latency_p99_ms=_f(percentiles, "p99") * 1000,
                            success_rate=success_rate,
                        )
                        with Session(get_engine()) as session:
                            session.add(sr)
                            session.commit()
                        logger.info(
                            "[%s] %s/%s: %.0f rps, p50=%.2fms, success=%.1f%%",
                            run_id[:8],
                            env_name,
                            scenario.name,
                            rps,
                            _f(percentiles, "p50") * 1000,
                            success_rate * 100,
                        )
                    else:
                        logger.warning(
                            "[%s] %s/%s: 0%% success rate — skipping",
                            run_id[:8],
                            env_name,
                            scenario.name,
                        )
                else:
                    logger.warning(
                        "[%s] %s/%s: oha returned no output",
                        run_id[:8],
                        env_name,
                        scenario.name,
                    )

                completed += 1

        # ---- Profiling pass ----
        if config.profile:
            for env_name in env_names:
                url = env_urls[env_name]
                logger.info("[%s] === Profiling pass: %s ===", run_id[:8], env_name)
                token = _get_token(ws)

                await _reset_items(url, token)
                await _reset_profiling(url, token)

                for scenario in PROFILE_SCENARIOS:
                    if _is_cancelled(run_id):
                        raise RuntimeError("Run cancelled")

                    _set_progress(
                        run_id, total, completed, f"profile {env_name}/{scenario.name}"
                    )

                    await _run_oha(
                        oha_path,
                        scenario,
                        url,
                        token,
                        config.duration,
                        config.connections,
                    )
                    completed += 1

                # Extract profiling JSONL.
                jsonl_text = await _extract_profiling(url, token)
                scheduler_stats = await _collect_scheduler_stats(url, token)

                if jsonl_text:
                    pr = ProfileResult(
                        run_id=run_id,
                        environment=env_name,
                        raw_jsonl=jsonl_text,
                    )
                    with Session(get_engine()) as session:
                        session.add(pr)
                        session.commit()
                    logger.info(
                        "[%s] Stored profiling data for %s", run_id[:8], env_name
                    )
                if scheduler_stats:
                    logger.info(
                        "[%s] Scheduler stats for %s: %s",
                        run_id[:8],
                        env_name,
                        json.dumps(scheduler_stats),
                    )

        # ---- Generate report ----
        _set_progress(run_id, total, total, "generating report")
        logger.info("[%s] Generating report...", run_id[:8])

        with Session(get_engine()) as session:
            run = session.get(BenchmarkRun, run_id)
            scenario_results = list(run.results) if run else []
            profile_results_list = list(run.profiles) if run else []

        report = generate_report(config, scenario_results, profile_results_list)

        elapsed = time.monotonic() - run_start
        _update_run(
            run_id,
            status=RunStatus.COMPLETED,
            report_json=json.dumps(report),
            progress_json=json.dumps(
                {"total": total, "completed": total, "current": "done"}
            ),
        )
        logger.info("[%s] Run completed in %.1fs", run_id[:8], elapsed)

    except Exception as exc:
        elapsed = time.monotonic() - run_start
        logger.exception("[%s] Run failed after %.1fs", run_id[:8], elapsed)
        _update_run(run_id, status=RunStatus.FAILED, error_message=str(exc))
    finally:
        _cancel_flags.pop(run_id, None)
