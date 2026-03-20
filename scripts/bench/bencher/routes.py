"""API endpoints for the bencher service."""

from __future__ import annotations

import json
import logging

from fastapi import APIRouter, BackgroundTasks, Depends, HTTPException, Request
from sqlmodel import Session, select

from .database import get_session
from .models import (
    BenchConfig,
    BenchmarkRun,
    BenchRunDetailResponse,
    BenchRunResponse,
    RunStatus,
)
from .runner import execute_run, request_cancel

logger = logging.getLogger("bencher.routes")

router = APIRouter(prefix="/api")


@router.get("/health")
async def health():
    return {"status": "ok"}


@router.post("/benchmarks", status_code=202, response_model=BenchRunResponse)
async def start_benchmark(
    config: BenchConfig,
    request: Request,
    background_tasks: BackgroundTasks,
    session: Session = Depends(get_session),
):
    """Start a new benchmark run."""
    mode = "bench+profile" if config.profile else "bench"
    run = BenchmarkRun(
        name=config.name,
        status=RunStatus.PENDING,
        mode=mode,
        config_json=config.model_dump_json(),
        progress_json=json.dumps({"total": 0, "completed": 0, "current": "pending"}),
    )
    session.add(run)
    session.commit()
    session.refresh(run)

    oha_path: str = request.app.state.oha_path

    logger.info(
        "Scheduling benchmark run %s (name=%s, mode=%s, envs=%s)",
        run.id,
        config.name,
        mode,
        list(config.environments.keys()),
    )
    background_tasks.add_task(execute_run, run.id, config, oha_path)

    return BenchRunResponse(
        id=run.id,
        name=run.name,
        status=run.status,
        mode=run.mode,
        progress=json.loads(run.progress_json),
        error_message=run.error_message,
        created_at=run.created_at,
        updated_at=run.updated_at,
    )


@router.get("/benchmarks", response_model=list[BenchRunResponse])
async def list_benchmarks(
    name: str | None = None,
    session: Session = Depends(get_session),
):
    """List benchmark runs, optionally filtered by name."""
    stmt = select(BenchmarkRun).order_by(BenchmarkRun.created_at.desc())  # type: ignore[attr-defined]
    if name:
        stmt = stmt.where(BenchmarkRun.name == name)
    runs = session.exec(stmt).all()
    logger.info("Listing %d benchmark runs (name=%s)", len(runs), name)
    return [
        BenchRunResponse(
            id=r.id,
            name=r.name,
            status=r.status,
            mode=r.mode,
            progress=json.loads(r.progress_json),
            error_message=r.error_message,
            created_at=r.created_at,
            updated_at=r.updated_at,
        )
        for r in runs
    ]


@router.get("/benchmarks/{run_id}", response_model=BenchRunDetailResponse)
async def get_benchmark(run_id: str, session: Session = Depends(get_session)):
    """Get benchmark run details."""
    run = session.get(BenchmarkRun, run_id)
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")

    return BenchRunDetailResponse(
        id=run.id,
        name=run.name,
        status=run.status,
        mode=run.mode,
        progress=json.loads(run.progress_json),
        error_message=run.error_message,
        created_at=run.created_at,
        updated_at=run.updated_at,
        report=json.loads(run.report_json) if run.report_json else None,
    )


@router.get("/benchmarks/{run_id}/report")
async def get_report(run_id: str, session: Session = Depends(get_session)):
    """Get full report JSON for a completed run."""
    run = session.get(BenchmarkRun, run_id)
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")
    if not run.report_json:
        raise HTTPException(status_code=404, detail="Report not available yet")
    logger.info("Serving report for run %s", run_id)
    return json.loads(run.report_json)


@router.delete("/benchmarks/{run_id}")
async def cancel_benchmark(run_id: str, session: Session = Depends(get_session)):
    """Cancel a running benchmark (best-effort)."""
    run = session.get(BenchmarkRun, run_id)
    if not run:
        raise HTTPException(status_code=404, detail="Run not found")
    if run.status not in (RunStatus.PENDING, RunStatus.RUNNING):
        raise HTTPException(
            status_code=400, detail=f"Run is {run.status}, cannot cancel"
        )

    logger.info("Cancelling benchmark run %s", run_id)
    cancelled = request_cancel(run_id)
    if not cancelled:
        # Not tracked yet or already finished — mark directly.
        run.status = RunStatus.FAILED
        run.error_message = "Cancelled"
        session.add(run)
        session.commit()

    return {"status": "cancelling"}
