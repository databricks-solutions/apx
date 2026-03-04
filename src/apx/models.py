"""Thin re-export of framework models for ``from apx.models import ...``."""

from apx._core import Request, Response
from apx._framework.models import RequestModel, ResponseModel

__all__ = ["RequestModel", "ResponseModel", "Response", "Request"]
