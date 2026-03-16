"""SQLModel tables and Pydantic schemas for the bencher service."""
# NOTE: Do NOT use `from __future__ import annotations` here —
# SQLAlchemy needs runtime evaluation of type annotations for relationships.

from datetime import datetime, timezone
from enum import Enum
from typing import Optional
from uuid import uuid4

from pydantic import BaseModel
from sqlmodel import Field, Relationship, SQLModel


# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------


class RunStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"


# ---------------------------------------------------------------------------
# SQLModel tables
# ---------------------------------------------------------------------------


class BenchmarkRun(SQLModel, table=True):
    __tablename__ = "benchmark_runs"

    id: str = Field(default_factory=lambda: uuid4().hex, primary_key=True)
    name: str = Field(index=True)
    status: RunStatus = Field(default=RunStatus.PENDING)
    mode: str = "bench"
    config_json: str = "{}"
    progress_json: str = "{}"
    error_message: Optional[str] = None
    report_json: Optional[str] = None
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    updated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    results: list["ScenarioResult"] = Relationship(back_populates="run")
    profiles: list["ProfileResult"] = Relationship(back_populates="run")


class ScenarioResult(SQLModel, table=True):
    __tablename__ = "scenario_results"

    id: str = Field(default_factory=lambda: uuid4().hex, primary_key=True)
    run_id: str = Field(foreign_key="benchmark_runs.id", index=True)
    environment: str
    scenario: str
    raw_oha_json: str
    requests_per_sec: float
    latency_p50_ms: float
    latency_p90_ms: float
    latency_p99_ms: float
    success_rate: float
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    run: Optional[BenchmarkRun] = Relationship(back_populates="results")


class ProfileResult(SQLModel, table=True):
    __tablename__ = "profile_results"

    id: str = Field(default_factory=lambda: uuid4().hex, primary_key=True)
    run_id: str = Field(foreign_key="benchmark_runs.id", index=True)
    environment: str
    raw_jsonl: str
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    run: Optional[BenchmarkRun] = Relationship(back_populates="profiles")


# ---------------------------------------------------------------------------
# Pydantic request / response schemas
# ---------------------------------------------------------------------------


class Scenario(BaseModel):
    """An HTTP scenario to benchmark."""

    name: str
    method: str
    path: str
    body: Optional[dict] = None


class BenchConfig(BaseModel):
    """Request body for POST /api/benchmarks."""

    name: str
    environments: dict[str, str]  # {"uvicorn": "bench-uvicorn", ...}
    scenarios: Optional[list[Scenario]] = None
    duration: str = "10s"
    connections: int = 100
    warmup_requests: int = 1000
    profile: bool = False


class BenchRunResponse(BaseModel):
    """Summary response for a benchmark run."""

    id: str
    name: str
    status: RunStatus
    mode: str
    progress: dict
    error_message: Optional[str]
    created_at: datetime
    updated_at: datetime


class BenchRunDetailResponse(BenchRunResponse):
    """Detailed response including report."""

    report: Optional[dict] = None
