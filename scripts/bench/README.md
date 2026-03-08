# APX vs Uvicorn Benchmark

Containerised throughput and latency comparison of APX against Uvicorn,
driven by [oha](https://github.com/hatoo/oha).

## Prerequisites

- **Docker** (daemon running)
- **oha** -- `brew install oha`
- **uv** -- `brew install uv` or see <https://docs.astral.sh/uv/>

## Quick start

```bash
uv run scripts/bench/run_bench.py
```

## CLI options

| Flag | Default | Description |
|------|---------|-------------|
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

## How it works

1. Builds Docker images for each server (`Dockerfile.apx`, `Dockerfile.uvicorn`).
2. Starts one container at a time with the configured CPU/memory limits.
3. Waits for `/api/health` to return 200.
4. Runs `oha` against every scenario defined in `scenarios.json`.
5. Stops the container, then repeats for the next server.
6. Generates a comparison report (terminal + markdown).

## Output

Raw results are written to:

```
scripts/bench/results/{server}/{scenario}.json
```

A markdown summary is written to `scripts/bench/results/report.md`.

## Regenerating the report

To regenerate the report from existing results without re-running benchmarks:

```bash
uv run scripts/bench/report.py
```

The report script accepts `--results-dir`, `--output`, and
`--format` (`terminal`, `markdown`, or `both`).
