"""Request and response model base classes for apx handlers.

- ``ResponseModel``: Pydantic BaseModel with camelCase serialization by default.
- ``RequestModel``: Pydantic BaseModel for JSON request body validation.
- ``Response``: Raw response with explicit body/status/headers control (Rust #[pyclass]).
- ``Request``: Full HTTP request object (Rust #[pyclass]).
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict
from pydantic.alias_generators import to_camel

# Re-export Rust-defined types so existing `from .models import Request, Response` works.
from apx._core import Request, Response


class ResponseModel(BaseModel):
    """Base class for structured API responses.

    Fields are serialized to camelCase by default via Pydantic alias generation.
    Use ``model_dump_json(by_alias=True)`` for JSON output (done automatically
    by the Rust runtime).
    """

    model_config = ConfigDict(
        alias_generator=to_camel,
        populate_by_name=True,
    )


class RequestModel(BaseModel):
    """Base class for JSON request body validation.

    The Rust runtime calls ``Model.model_validate_json(body)`` on the raw
    request bytes — no intermediate dict, single-pass via pydantic-core.
    """
