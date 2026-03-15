# APX vs Uvicorn Benchmark

Containerised throughput and latency comparison of APX against Uvicorn,
driven by [oha](https://github.com/hatoo/oha).

Three environments are compared:

| Environment | Description |
|-------------|-------------|
| `uvicorn` | Uvicorn + uvloop + httptools |
| `apx-asyncio` | APX + asyncio |
| `apx-uvloop` | APX + uvloop |

## Prerequisites

- **Docker** (daemon running)
- **oha** -- `brew install oha`
- **uv** -- `brew install uv` or see <https://docs.astral.sh/uv/>

## Quick start

```bash
uv run scripts/bench/run_bench.py --name my-run
```

## CLI options

| Flag | Default | Description |
|------|---------|-------------|
| `--name` | *(required)* | Run name (results stored under `results/<name>/`) |
| `-d`, `--duration` | `30s` | Duration per scenario (`oha -z`) |
| `-c`, `--connections` | `100` | Concurrent connections |
| `--cpus` | `2` | CPU limit for containers |
| `--memory` | `4g` | Memory limit for containers |
| `--port` | `8000` | Host port to map |
| `--server` | `both` | `uvicorn`, `apx`, or `both` |
| `--skip-build` | off | Reuse existing Docker images |
| `--no-report` | off | Skip report generation after run |
| `--results-dir` | `scripts/bench/results` | Where to write raw JSON |
| `--scenarios` | `scripts/bench/scenarios.json` | Scenario definitions |
| `--warmup` | `1000` | Warmup requests before benchmarking |
| `--tokio-threads` | *(auto)* | Set `TOKIO_WORKER_THREADS` in APX container |
| `--compare` | off | Run 3-way comparison (uvicorn vs APX+asyncio vs APX+uvloop) |
| `--profile` | off | Run profiling: measures Python-level per-request timing |
| `--profile-duration` | `15s` | Duration for profiling load |
| `--sweep` | off | Run echo scenario across worker/thread/connection matrix |

## How it works

1. Builds Docker images for each server (`Dockerfile.apx`, `Dockerfile.uvicorn`).
2. Starts one container at a time with the configured CPU/memory limits.
3. Waits for `/api/health` to return 200.
4. Runs `oha` against every scenario defined in `scenarios.json`.
5. Stops the container, then repeats for the next server.
6. Generates a comparison report (terminal + JSON).

## Output

Raw results are written to:

```
scripts/bench/results/<name>/environments/<env>/<scenario>.json
```

A JSON report is written to `scripts/bench/results/<name>/report.json`.
