"""apx._framework — internal Python surface API.

This subpackage provides the user-facing Python API for the apx framework:
route decorators, request/response models, and error classes.

The Rust runtime imports this package via PyO3 to discover routes and
dispatch requests. Users import from ``apx`` (not ``apx._framework``).
"""

from .app import App
from .models import RequestModel, ResponseModel

# Re-export Rust-defined types for backward compatibility.
from apx._core import (
    BadRequest,
    Forbidden,
    NotFound,
    Request,
    Response,
)

__all__ = [
    "App",
    "BadRequest",
    "Forbidden",
    "NotFound",
    "Request",
    "RequestModel",
    "Response",
    "ResponseModel",
]
