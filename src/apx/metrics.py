"""APX Metrics — counters, histograms, gauges exported via OTLP.

Usage::

    from apx.metrics import counter, histogram

    requests = counter("requests_total", description="Total requests")
    requests.inc()
    requests.inc(labels={"method": "GET"})

    latency = histogram("request_latency_seconds")
    latency.observe(0.042)
"""

from __future__ import annotations

from typing import Any

from apx._core import (
    RustCounter,
    RustGauge,
    RustHistogram,
    create_counter as _create_counter,
    create_gauge as _create_gauge,
    create_histogram as _create_histogram,
)


class Counter:
    """An OTLP counter metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustCounter = _create_counter(name, description, unit)

    def inc(self, value: int = 1, *, labels: dict[str, str] | None = None) -> None:
        """Increment the counter."""
        self._instrument.inc(value, labels)


class Histogram:
    """An OTLP histogram metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustHistogram = _create_histogram(name, description, unit)

    def observe(
        self, value: float, *, labels: dict[str, str] | None = None
    ) -> None:
        """Record an observation."""
        self._instrument.observe(value, labels)


class Gauge:
    """An OTLP gauge metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustGauge = _create_gauge(name, description, unit)

    def set(self, value: float, *, labels: dict[str, str] | None = None) -> None:
        """Set the gauge value."""
        self._instrument.set(value, labels)


def counter(name: str, *, description: str = "", unit: str = "") -> Counter:
    """Create a counter metric."""
    return Counter(name, description=description, unit=unit)


def histogram(name: str, *, description: str = "", unit: str = "") -> Histogram:
    """Create a histogram metric."""
    return Histogram(name, description=description, unit=unit)


def gauge(name: str, *, description: str = "", unit: str = "") -> Gauge:
    """Create a gauge metric."""
    return Gauge(name, description=description, unit=unit)
