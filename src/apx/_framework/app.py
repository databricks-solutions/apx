"""The ``App`` class — the entry point for apx framework applications.

Provides route decorators (``@app.get``, ``@app.post``, etc.), app
composition via ``mount()``, and configuration (``max_body_limit``).
"""

from __future__ import annotations

import asyncio
import inspect
import re
from typing import Any, Callable, TypeVar, get_type_hints

from apx._core import ParamInfo, RouteInfo
from apx._core import PyHttpMethod as HttpMethod

# Matches path template variables: {item_id}, {user_id}, etc.
_PATH_PARAM_RE = re.compile(r"\{(\w+)\}")

F = TypeVar("F", bound=Callable[..., Any])


def _resolve_params(handler: Callable[..., Any], path: str) -> list[ParamInfo]:
    """Resolve handler parameters to their sources.

    Resolution order:
    1. Name matches a path variable → Path
    2. Type is ``bytes`` → RawBody
    3. Type is ``Request`` → RawRequest
    4. Type is a ``RequestModel`` subclass → Body
    5. Otherwise → Query
    """
    from .models import Request, RequestModel

    path_vars = set(_PATH_PARAM_RE.findall(path))
    sig = inspect.signature(handler)
    hints = get_type_hints(handler)
    params: list[ParamInfo] = []

    for name, param in sig.parameters.items():
        ann = hints.get(name)
        if ann is None:
            continue

        required = param.default is inspect.Parameter.empty
        qualname = _type_qualname(ann)

        source = _classify_param(name, ann, path_vars)
        params.append(ParamInfo(
            name=name,
            type_qualname=qualname,
            source=source,
            required=required,
        ))

    return params


def _classify_param(
    name: str,
    ann: type,
    path_vars: set[str],
) -> str:
    """Classify a single parameter to its source."""
    from .models import Request, RequestModel

    if name in path_vars:
        return "path"
    if ann is bytes:
        return "raw_body"
    if ann is Request:
        return "raw_request"
    if isinstance(ann, type) and issubclass(ann, RequestModel):
        return "body"
    return "query"


def _type_qualname(ann: Any) -> str:
    """Get the qualified name of a type annotation."""
    if ann is None:
        return "None"
    if isinstance(ann, type):
        module = getattr(ann, "__module__", "")
        qualname = getattr(ann, "__qualname__", str(ann))
        if module in ("builtins", ""):
            return qualname
        return f"{module}.{qualname}"
    return str(ann)


def _resolve_response_type(handler: Callable[..., Any]) -> str:
    """Resolve the handler's return type annotation.

    Returns ``"model:<qualname>"`` for ResponseModel subclasses,
    or ``"raw_response"`` for ``Response`` or untyped returns.
    """
    from .models import Response, ResponseModel

    hints = get_type_hints(handler)
    ret = hints.get("return")
    if ret is None:
        return "raw_response"
    if isinstance(ret, type) and issubclass(ret, ResponseModel):
        return f"model:{_type_qualname(ret)}"
    if isinstance(ret, type) and issubclass(ret, Response):
        return "raw_response"
    return "raw_response"


class App:
    """The apx application.

    Create an instance, register routes with decorators, and let the Rust
    runtime serve them.

    Args:
        max_body_limit: Maximum request body size in bytes. Default 1 MiB.
    """

    def __init__(self, max_body_limit: int = 1024 * 1024) -> None:
        self._routes: list[RouteInfo] = []
        self._max_body_limit = max_body_limit
        self._tags: list[str] = []

    def _register(
        self,
        method: HttpMethod,
        path: str,
        handler: Callable[..., Any],
        tags: list[str] | None = None,
    ) -> None:
        """Register a route handler."""
        if not asyncio.iscoroutinefunction(handler):
            raise TypeError(
                f"Handler '{handler.__name__}' must be an async def, "
                f"got sync function"
            )

        params = _resolve_params(handler, path)
        response_type = _resolve_response_type(handler)
        route_tags = list(self._tags)
        if tags:
            route_tags.extend(tags)

        self._routes.append(RouteInfo(
            method=method,
            path=path,
            handler=handler,
            handler_qualname=_type_qualname(handler),
            params=params,
            response_type=response_type,
            tags=route_tags,
        ))

    def get(self, path: str, *, tags: list[str] | None = None) -> Callable[[F], F]:
        """Register a GET route."""
        def decorator(handler: F) -> F:
            self._register(HttpMethod.Get, path, handler, tags)
            return handler
        return decorator

    def post(self, path: str, *, tags: list[str] | None = None) -> Callable[[F], F]:
        """Register a POST route."""
        def decorator(handler: F) -> F:
            self._register(HttpMethod.Post, path, handler, tags)
            return handler
        return decorator

    def put(self, path: str, *, tags: list[str] | None = None) -> Callable[[F], F]:
        """Register a PUT route."""
        def decorator(handler: F) -> F:
            self._register(HttpMethod.Put, path, handler, tags)
            return handler
        return decorator

    def delete(self, path: str, *, tags: list[str] | None = None) -> Callable[[F], F]:
        """Register a DELETE route."""
        def decorator(handler: F) -> F:
            self._register(HttpMethod.Delete, path, handler, tags)
            return handler
        return decorator

    def patch(self, path: str, *, tags: list[str] | None = None) -> Callable[[F], F]:
        """Register a PATCH route."""
        def decorator(handler: F) -> F:
            self._register(HttpMethod.Patch, path, handler, tags)
            return handler
        return decorator

    def mount(self, prefix: str, sub_app: App) -> None:
        """Mount a sub-application at a path prefix.

        All routes from ``sub_app`` are merged into this app with the
        prefix prepended to each route path. Tags from the sub-app are
        preserved.
        """
        for route in sub_app._routes:
            merged_path = prefix.rstrip("/") + route.path
            merged_tags = list(sub_app._tags) + route.tags
            self._routes.append(RouteInfo(
                method=route.method,
                path=merged_path,
                handler=route.handler,
                handler_qualname=route.handler_qualname,
                params=route.params,
                response_type=route.response_type,
                tags=merged_tags,
            ))
