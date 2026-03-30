from __future__ import annotations

import asyncio

import diskcache
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import Response, StreamingResponse

from .models import Item, ItemCreate, ItemUpdate

router = APIRouter()

_CACHE_DIR = "/tmp/bench_items_cache"
_cache = diskcache.Cache(_CACHE_DIR)

_DEFAULT_ITEMS = [
    Item(
        id=i,
        name=f"Item {i}",
        description=f"Description for item {i}",
        price=round(i * 9.99, 2),
        tags=[f"tag-{i % 3}", f"tag-{i % 5}"],
    )
    for i in range(1, 11)
]


def _populate_defaults():
    _cache.clear()
    for item in _DEFAULT_ITEMS:
        _cache[f"item:{item.id}"] = item.model_dump()
    _cache["_counter"] = 10


# Auto-populate on first boot
if "_counter" not in _cache:
    _populate_defaults()


def _next_id() -> int:
    return _cache.incr("_counter")


@router.get("/version")
def version() -> dict[str, str]:
    """Return the APX package version (includes build timestamp)."""
    try:
        from importlib.metadata import version as pkg_version

        return {"apx": pkg_version("apx")}
    except Exception as exc:
        return {"apx": f"unknown ({exc})"}


@router.get("/echo")
def echo() -> dict[str, bool]:
    """Minimal handler — isolates framework overhead from app logic."""
    return {"echo": True}


@router.get("/request-id")
def request_id(request: Request) -> dict[str, str | None]:
    """Return the X-Request-Id seen by the ASGI app."""
    return {"request_id": request.headers.get("x-request-id")}


@router.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@router.get("/items", response_model=list[Item])
def list_items() -> list[Item]:
    items = []
    for key in _cache:
        if isinstance(key, str) and key.startswith("item:"):
            items.append(Item(**_cache[key]))
    items.sort(key=lambda x: x.id)
    return items


@router.get("/items/{item_id}", response_model=Item)
def get_item(item_id: int) -> Item:
    data = _cache.get(f"item:{item_id}")
    if data is None:
        raise HTTPException(status_code=404, detail="Item not found")
    return Item(**data)


@router.post("/items", response_model=Item, status_code=201)
def create_item(body: ItemCreate) -> Item:
    item = Item(id=_next_id(), **body.model_dump())
    _cache[f"item:{item.id}"] = item.model_dump()
    return item


@router.patch("/items/{item_id}", response_model=Item)
def update_item(item_id: int, body: ItemUpdate) -> Item:
    data = _cache.get(f"item:{item_id}")
    if data is None:
        raise HTTPException(status_code=404, detail="Item not found")
    existing = Item(**data)
    updated = existing.model_copy(update=body.model_dump(exclude_unset=True))
    _cache[f"item:{item_id}"] = updated.model_dump()
    return updated


@router.delete("/items/{item_id}", status_code=204)
def delete_item(item_id: int):
    _cache.pop(f"item:{item_id}", None)
    from fastapi.responses import Response

    return Response(status_code=204)


@router.post("/items/reset")
def items_reset():
    """Clear all items and repopulate with defaults."""
    _populate_defaults()
    return {"status": "reset", "items": 10}


# ---------------------------------------------------------------------------
# Diverse route types for scheduler/pipeline benchmarking
# ---------------------------------------------------------------------------


async def dep_level_c():
    await asyncio.sleep(0)
    return "c"


async def dep_level_b(c: str = Depends(dep_level_c)):
    await asyncio.sleep(0)
    return f"b({c})"


async def dep_level_a(b: str = Depends(dep_level_b)):
    return f"a({b})"


@router.get("/yield-once")
async def yield_once():
    """Scheduler yield/resume round-trip."""
    await asyncio.sleep(0)
    return {"yielded": True}


@router.get("/cpu/{n}")
def cpu_work(n: int):
    """GIL hold under concurrency — sum of squares."""
    n = min(n, 1_000_000)
    total = sum(i * i for i in range(n))
    return {"n": n, "result": total}


@router.get("/large/{kb}")
def large_response(kb: int):
    """Large body — send() overhead."""
    kb = min(kb, 1024)
    data = "x" * (kb * 1024)
    return Response(content=data, media_type="text/plain")


@router.post("/upload")
async def upload(request: Request):
    """Body collection path."""
    body = await request.body()
    return {"size": len(body)}


@router.get("/stream/{chunks}")
async def stream_response(chunks: int):
    """Streaming response with yields between chunks."""
    chunks = min(chunks, 10_000)

    async def generate():
        for i in range(chunks):
            yield f"chunk-{i}\n"
            await asyncio.sleep(0)

    return StreamingResponse(generate(), media_type="text/plain")


@router.get("/deps")
async def deps_endpoint(a: str = Depends(dep_level_a)):
    """3-level dependency chain — DI + coroutine stack depth."""
    return {"chain": a}


# ---------------------------------------------------------------------------
# Telemetry test endpoint
# ---------------------------------------------------------------------------


@router.get("/telemetry/test")
async def telemetry_test():
    """Exercise all OTEL telemetry surfaces for integration testing."""
    import logging

    from apx.telemetry import Counter, Gauge, Histogram, log, span

    with span("test.custom_span", surface="span"):
        pass

    counter = Counter(
        "test.custom_counter", description="integration test counter", unit="1"
    )
    counter.inc(1)

    histogram = Histogram(
        "test.custom_histogram", description="integration test histogram", unit="ms"
    )
    histogram.observe(42.0)

    gauge = Gauge("test.custom_gauge", description="integration test gauge")
    gauge.set(7.0)

    log.info("integration test log message")

    logging.getLogger("test.telemetry").info("integration test log message")

    return {"ok": True}


@router.get("/telemetry/nested-spans")
async def telemetry_nested_spans():
    """Three levels of nested spans for parent-child relationship testing."""
    from apx.telemetry import span

    with span("test.outer", depth="1"):
        with span("test.middle", depth="2"):
            with span("test.inner", depth="3"):
                pass

    return {"ok": True}


@router.get("/telemetry/sequential-spans")
async def telemetry_sequential_spans():
    """Parent span with two sequential children for sibling relationship testing."""
    from apx.telemetry import span

    with span("test.parent", role="parent"):
        with span("test.sibling_a", order="first"):
            pass
        with span("test.sibling_b", order="second"):
            pass

    return {"ok": True}


@router.get("/telemetry/error-handling")
async def telemetry_error_handling():
    """Exercise error capture: exception in span, log.exception, explicit status."""
    from apx.telemetry import StatusCode, log, span

    # (a) Span that catches a raised exception.
    try:
        with span("test.erroring_span"):
            raise ValueError("deliberate test error")
    except ValueError:
        pass

    # (b) log.exception() inside an except block.
    try:
        raise RuntimeError("log exception test")
    except RuntimeError:
        log.exception("caught runtime error", source="test")

    # (c) Span with explicit status set to Error.
    with span("test.explicit_error") as s:
        s.set_status(StatusCode.Error, "manually set error")

    # (d) Clean span for comparison.
    with span("test.clean_span"):
        pass

    return {"ok": True}


@router.get("/telemetry/otlp-fields")
async def telemetry_otlp_fields():
    """Exercise all OTLP field improvements for integration testing."""
    import logging

    from apx.telemetry import Counter, Gauge, Histogram, SpanKind, log, span

    with span("test.client_call", kind=SpanKind.CLIENT, target="upstream") as s:
        s.add_event("dns.resolved", {"host": "example.com"})
        s.set_attribute("net.peer.name", "example.com")

    with span("test.internal_work") as s:
        s.add_event("step.completed", {"step": "1"})

    log.info("otlp fields test log", event_name="test.otlp_fields")

    logging.getLogger("test.otlp_fields").info(
        "otlp fields test log",
        extra={"event_name": "test.otlp_fields"},
    )

    counter = Counter(
        "test.otlp_fields_counter", description="OTLP fields test", unit="1"
    )
    counter.inc(1)

    histogram = Histogram(
        "test.otlp_fields_histogram", description="OTLP fields test", unit="ms"
    )
    histogram.observe(42.0)

    gauge = Gauge("test.otlp_fields_gauge", description="OTLP fields test")
    gauge.set(7.0)

    return {"ok": True}


@router.get("/telemetry/event-name")
async def telemetry_event_name():
    """Exercise event_name on log methods and stdlib logging."""
    import logging

    from apx.telemetry import log

    log.info("user logged in", event_name="user.login", uid="42")
    log.warn("rate limit near", event_name="rate_limit.warning", current="950")

    logging.getLogger("test.event_name").warning(
        "stdlib with event_name",
        extra={"event_name": "stdlib.test_event"},
    )

    return {"ok": True}


@router.get("/telemetry/cross-signal")
async def telemetry_cross_signal():
    """Exercises spans, log-level spans, metrics, and stdlib logging together."""
    import logging

    from apx.telemetry import Counter, Histogram, log, span

    with span("test.cross_signal_span", surface="cross"):
        log.info("cross signal info log", signal="log_info")
        log.warn("cross signal warn log", signal="log_warn")

        counter = Counter(
            "test.cross_signal_counter",
            description="cross signal test counter",
            unit="1",
        )
        counter.inc(1, attributes={"scenario": "cross_signal"})

        histogram = Histogram(
            "test.cross_signal_histogram",
            description="cross signal test histogram",
            unit="ms",
        )
        histogram.observe(42.0, attributes={"scenario": "cross_signal"})

        logging.getLogger("test.cross_signal").warning(
            "cross signal stdlib warning"
        )

    return {"ok": True}


# ---------------------------------------------------------------------------
# Profiling / trace endpoints
# ---------------------------------------------------------------------------


@router.get("/profile/dump")
def profile_dump():
    """Return profiling JSONL over HTTP (for remote extraction)."""
    from .profiling import PROFILE_PATH, flush

    flush()
    if not PROFILE_PATH.exists():
        raise HTTPException(status_code=404, detail="No profiling data")
    return Response(content=PROFILE_PATH.read_text(), media_type="application/x-ndjson")


@router.delete("/profile/reset")
def profile_reset():
    """Clear profiling data for a fresh run."""
    from . import profiling

    profiling.reset()
    return {"status": "reset"}


@router.get("/_bench/scheduler-stats")
def scheduler_stats():
    """Return scheduler counters as JSON."""
    try:
        from apx._core import scheduler_stats_json
    except ImportError:
        raise HTTPException(status_code=404, detail="not running under APX")
    data = scheduler_stats_json()
    if data is None:
        raise HTTPException(status_code=404, detail="no scheduler stats")
    import json

    return json.loads(data)
