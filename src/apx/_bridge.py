"""Internal Rust-to-Python bridge utilities.

Functions in this module are called from the Rust framework crate via
``py.import(c"apx._bridge")``.  They are **not** part of the public API.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any


class _ApxHandler(logging.Handler):
    def __init__(self, emit_fn: Callable[[int, str, str, str], None]) -> None:
        super().__init__()
        self._emit = emit_fn

    def emit(self, record: logging.LogRecord) -> None:
        try:
            event_name = getattr(record, "event_name", "") or ""
            self._emit(record.levelno, record.getMessage(), record.name, event_name)
        except Exception:
            pass


def install_log_handler(emit_fn: Callable[[int, str, str, str], None]) -> None:
    handler = _ApxHandler(emit_fn)
    logging.root.addHandler(handler)
    logging.root.setLevel(logging.DEBUG)


async def resolved(val: Any) -> Any:
    return val
