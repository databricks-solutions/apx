"""Pydantic models mirroring the full OTLP JSON export structure.

The OTEL Collector file exporter writes one JSON object per JSONL line.
Each line is a complete export envelope (TracesExport / MetricsExport /
LogsExport).  Models parse the entire envelope via ``model_validate_json``
-- zero manual ``.get()`` / dict-walking required.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, TypeVar

from pydantic import BaseModel


# ---------------------------------------------------------------------------
# Shared primitives
# ---------------------------------------------------------------------------


class AnyValue(BaseModel):
    stringValue: str | None = None
    intValue: str | None = None
    doubleValue: float | None = None
    boolValue: bool | None = None
    arrayValue: dict[str, Any] | None = None
    kvlistValue: dict[str, Any] | None = None


class KeyValue(BaseModel):
    key: str
    value: AnyValue = AnyValue()


class Resource(BaseModel):
    attributes: list[KeyValue] = []


class InstrumentationScope(BaseModel):
    name: str = ""
    version: str = ""
    schemaUrl: str = ""
    attributes: list[KeyValue] = []


# ---------------------------------------------------------------------------
# Traces
# ---------------------------------------------------------------------------


class SpanStatus(BaseModel):
    code: int = 0
    message: str = ""


class SpanEvent(BaseModel):
    name: str = ""
    timeUnixNano: str = ""
    attributes: list[KeyValue] = []


class Span(BaseModel):
    traceId: str = ""
    spanId: str = ""
    parentSpanId: str = ""
    name: str = ""
    kind: int = 0
    startTimeUnixNano: str = ""
    endTimeUnixNano: str = ""
    attributes: list[KeyValue] = []
    events: list[SpanEvent] = []
    status: SpanStatus = SpanStatus()
    flags: int = 0
    traceState: str = ""


class ScopeSpans(BaseModel):
    scope: InstrumentationScope = InstrumentationScope()
    spans: list[Span] = []
    schemaUrl: str = ""


class ResourceSpans(BaseModel):
    resource: Resource = Resource()
    scopeSpans: list[ScopeSpans] = []
    schemaUrl: str = ""


class TracesExport(BaseModel):
    resourceSpans: list[ResourceSpans] = []


# ---------------------------------------------------------------------------
# Logs
# ---------------------------------------------------------------------------


class LogRecord(BaseModel):
    timeUnixNano: str = ""
    observedTimeUnixNano: str = ""
    severityNumber: int = 0
    severityText: str = ""
    body: AnyValue = AnyValue()
    attributes: list[KeyValue] = []
    traceId: str = ""
    spanId: str = ""
    flags: int = 0
    eventName: str = ""


class ScopeLogs(BaseModel):
    scope: InstrumentationScope = InstrumentationScope()
    logRecords: list[LogRecord] = []
    schemaUrl: str = ""


class ResourceLogs(BaseModel):
    resource: Resource = Resource()
    scopeLogs: list[ScopeLogs] = []
    schemaUrl: str = ""


class LogsExport(BaseModel):
    resourceLogs: list[ResourceLogs] = []


# ---------------------------------------------------------------------------
# Metrics
# ---------------------------------------------------------------------------


class DataPoint(BaseModel):
    attributes: list[KeyValue] = []
    asInt: str | None = None
    asDouble: float | None = None
    startTimeUnixNano: str = ""
    timeUnixNano: str = ""
    count: str | None = None
    sum: float | None = None
    bucketCounts: list[str] = []
    explicitBounds: list[float] = []


class MetricData(BaseModel):
    dataPoints: list[DataPoint] = []
    aggregationTemporality: int | None = None
    isMonotonic: bool | None = None


class Metric(BaseModel):
    name: str = ""
    description: str = ""
    unit: str = ""
    sum: MetricData | None = None
    gauge: MetricData | None = None
    histogram: MetricData | None = None


class ScopeMetrics(BaseModel):
    scope: InstrumentationScope = InstrumentationScope()
    metrics: list[Metric] = []
    schemaUrl: str = ""


class ResourceMetrics(BaseModel):
    resource: Resource = Resource()
    scopeMetrics: list[ScopeMetrics] = []
    schemaUrl: str = ""


class MetricsExport(BaseModel):
    resourceMetrics: list[ResourceMetrics] = []


# ---------------------------------------------------------------------------
# Generic JSONL reader
# ---------------------------------------------------------------------------

T = TypeVar("T", TracesExport, LogsExport, MetricsExport)


def read_jsonl(path: Path, model: type[T]) -> list[T]:
    """Parse a JSONL file into a list of typed Pydantic export envelopes."""
    if not path.exists():
        return []
    results: list[T] = []
    for line in path.read_text().splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        results.append(model.model_validate(json.loads(stripped)))
    return results
