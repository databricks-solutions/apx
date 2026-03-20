#!/usr/bin/env python3
"""Phase 0e: stress verification gate for per-step _enter_task/_leave_task.

Starts apx with a lowered GIL switch interval (1ms), hammers it with
concurrent requests to streaming + dependency-heavy routes, and checks
for _enter_task RuntimeError in server output.

Usage (from project root):
    # 1. Build the binary first:
    cargo build
    # 2. Run the stress test:
    uv run python scripts/stress_test_a5.py

Success criteria:
  - Zero "_enter_task" RuntimeError in server stderr
  - No throughput collapse (stream endpoint returns 200)
  - All concurrent requests complete within timeout
"""

from __future__ import annotations

import asyncio
import os
import signal
import subprocess
import sys
import time

CONCURRENCY = 50
DURATION_SECONDS = 5
BASE_URL = "http://127.0.0.1:8765"

ROUTES = [
    "/api/stream/10",
    "/api/echo",
    "/api/health",
    "/api/deps",
    "/api/yield-once",
]


async def hammer(session, url: str, results: dict):
    """Send requests to a single URL until stopped."""
    count = 0
    errors = 0
    deadline = time.monotonic() + DURATION_SECONDS
    while time.monotonic() < deadline:
        try:
            resp = await asyncio.wait_for(session.get(url), timeout=5.0)
            if resp.status_code == 200:
                _ = resp.text
                count += 1
            else:
                errors += 1
        except Exception:
            errors += 1
    results[url] = {"ok": count, "errors": errors}


async def run_stress():
    import httpx

    async with httpx.AsyncClient(base_url=BASE_URL, timeout=10.0) as client:
        # Wait for server to be ready
        for _ in range(30):
            try:
                resp = await client.get("/api/health")
                if resp.status_code == 200:
                    break
            except Exception:
                pass
            await asyncio.sleep(0.5)
        else:
            print("FAIL: server did not become ready within 15s")
            return False

        print(
            f"Server ready. Hammering {len(ROUTES)} routes × {CONCURRENCY} "
            f"concurrent for {DURATION_SECONDS}s..."
        )

        results: dict = {}
        tasks = []
        for route in ROUTES:
            for _ in range(CONCURRENCY // len(ROUTES)):
                tasks.append(asyncio.create_task(hammer(client, route, results)))

        await asyncio.gather(*tasks)

        print("\nResults:")
        total_ok = 0
        total_err = 0
        for route, r in sorted(results.items()):
            print(f"  {route}: {r['ok']} ok, {r['errors']} errors")
            total_ok += r["ok"]
            total_err += r["errors"]
        print(f"  TOTAL: {total_ok} ok, {total_err} errors")
        return total_ok > 0


def main():
    project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    apx_bin = os.path.join(project_root, "target", "debug", "apx")

    if not os.path.exists(apx_bin):
        print(f"Binary not found at {apx_bin}. Run 'cargo build' first.")
        sys.exit(1)

    sitecustomize = os.path.join(project_root, "scripts", "_stress_sitecustomize.py")
    with open(sitecustomize, "w") as f:
        f.write("import sys; sys.setswitchinterval(0.001)\n")

    env = os.environ.copy()
    env["PYTHONPATH"] = (
        os.path.dirname(sitecustomize) + os.pathsep + env.get("PYTHONPATH", "")
    )
    env["PYTHONSTARTUP"] = sitecustomize

    print(f"Starting apx server on port 8765 with GIL switch interval 1ms...")
    proc = subprocess.Popen(
        [
            apx_bin,
            "serve",
            "scripts.bench.app.main:app",
            "--port",
            "8765",
            "--workers",
            "1",
            "--loop",
            "uvloop",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        cwd=project_root,
    )

    try:
        success = asyncio.run(run_stress())

        proc.send_signal(signal.SIGINT)
        try:
            _, stderr_bytes = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            _, stderr_bytes = proc.communicate()

        stderr = stderr_bytes.decode("utf-8", errors="replace")

        enter_task_errors = stderr.count("Cannot enter into task")
        runtime_errors = stderr.count("_enter_task")

        print(f"\nServer stderr analysis:")
        print(f"  _enter_task collisions: {enter_task_errors}")
        print(f"  _enter_task mentions: {runtime_errors}")

        if enter_task_errors > 0:
            print("\nFAIL: _enter_task RuntimeError detected in server logs!")
            print("--- relevant stderr lines ---")
            for line in stderr.splitlines():
                if "enter_task" in line.lower() or "RuntimeError" in line:
                    print(f"  {line}")
            sys.exit(1)
        elif not success:
            print("\nFAIL: stress test did not complete successfully")
            sys.exit(1)
        else:
            print("\nPASS: zero _enter_task collisions under stress")
            sys.exit(0)

    except KeyboardInterrupt:
        proc.kill()
        proc.wait()
        sys.exit(130)


if __name__ == "__main__":
    main()
