"""Thin re-export of framework errors for ``from apx.errors import ...``."""

from apx._core import BadRequest, Forbidden, NotFound

__all__ = ["NotFound", "BadRequest", "Forbidden"]
