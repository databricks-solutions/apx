"""Internal Rust-to-Python bridge utilities.

Functions in this module are called from the Rust framework crate via
``py.import(c"apx._bridge")``.  They are **not** part of the public API.
"""

from __future__ import annotations

import logging
import traceback
from collections.abc import Coroutine
from typing import Any, Callable, Protocol


class _ErrorSink(Protocol):
    def send_error(self, tb: str) -> None: ...


class _ApxHandler(logging.Handler):
    def __init__(self, emit_fn: Callable[[int, str, str], None]) -> None:
        super().__init__()
        self._emit = emit_fn

    def emit(self, record: logging.LogRecord) -> None:
        try:
            self._emit(record.levelno, record.getMessage(), record.name)
        except Exception:
            pass


def install_log_handler(emit_fn: Callable[[int, str, str], None]) -> None:
    handler = _ApxHandler(emit_fn)
    logging.root.addHandler(handler)
    logging.root.setLevel(logging.DEBUG)


async def resolved(val: Any) -> Any:
    return val


async def guarded(coro: Coroutine[Any, Any, None], send: _ErrorSink) -> None:
    try:
        await coro
    except Exception as exc:
        tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
        send.send_error(tb)
