"""APX Telemetry — distributed tracing spans.

Usage::

    from apx.telemetry import span

    with span("generate_report", report_id="123"):
        ...

    async with span("db_query"):
        await db.fetch()

    @span("load_user")
    async def load_user(id: int):
        ...
"""

from __future__ import annotations

import asyncio
import functools
from typing import Any, Callable, TypeVar

from apx._core import SpanHandle

_F = TypeVar("_F", bound=Callable[..., Any])


class span:
    """Context manager / decorator for creating trace spans."""

    def __init__(self, name: str, **attributes: Any) -> None:
        self._name = name
        self._attributes = {k: str(v) for k, v in attributes.items()}
        self._handle: SpanHandle | None = None

    def __enter__(self) -> SpanHandle:
        self._handle = SpanHandle(self._name, self._attributes)
        self._handle.__enter__()
        return self._handle

    def __exit__(self, *args: object) -> bool:
        if self._handle is not None:
            return self._handle.__exit__(*args)
        return False

    async def __aenter__(self) -> SpanHandle:
        self._handle = SpanHandle(self._name, self._attributes)
        self._handle.__enter__()  # ContextVars are async-safe
        return self._handle

    async def __aexit__(self, *args: object) -> bool:
        if self._handle is not None:
            return self._handle.__exit__(*args)
        return False

    def __call__(self, fn: _F) -> _F:
        if asyncio.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                async with span(self._name, **self._attributes):
                    return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            with span(self._name, **self._attributes):
                return fn(*args, **kwargs)

        return sync_wrapper  # type: ignore[return-value]
