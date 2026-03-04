from pathlib import Path
from typing import Any

__version__: str

def run_cli(args: list[str]) -> int: ...
def generate_openapi(project_root: Path) -> bool: ...
def get_dotenv_vars() -> dict[str, str]: ...

# ── Framework types ──────────────────────────────────────────────────────

class PyHttpMethod:
    """HTTP method enum."""

    Get: PyHttpMethod
    Post: PyHttpMethod
    Put: PyHttpMethod
    Delete: PyHttpMethod
    Patch: PyHttpMethod

    @property
    def value(self) -> str: ...

class ParamInfo:
    """Parameter metadata extracted from a handler's signature."""

    name: str
    type_qualname: str
    source: str
    required: bool

    def __init__(
        self,
        name: str,
        type_qualname: str,
        source: str,
        required: bool,
    ) -> None: ...

class RouteInfo:
    """Route metadata extracted from an App decorator."""

    method: PyHttpMethod
    path: str
    handler: Any
    handler_qualname: str
    params: list[ParamInfo]
    response_type: str
    tags: list[str]

    def __init__(
        self,
        method: PyHttpMethod,
        path: str,
        handler: Any,
        handler_qualname: str,
        params: list[ParamInfo] = ...,
        response_type: str = ...,
        tags: list[str] = ...,
    ) -> None: ...

class Request:
    """Full HTTP request object, constructed by Rust for RawRequest injection."""

    method: str
    path: str
    query_string: str
    headers: dict[str, str]
    cookies: dict[str, str]

    def __init__(
        self,
        *,
        method: str = ...,
        path: str = ...,
        query_string: str = ...,
        headers: dict[str, str] | None = ...,
        cookies: dict[str, str] | None = ...,
        _body: bytes | None = ...,
    ) -> None: ...

    async def body(self) -> bytes: ...

class Response:
    """Raw HTTP response with explicit control over body, status, and headers."""

    body: Any
    status: int
    headers: dict[str, str]

    def __init__(
        self,
        body: Any = ...,
        status: int = ...,
        headers: dict[str, str] | None = ...,
    ) -> None: ...

# ── Exceptions ───────────────────────────────────────────────────────────

class NotFound(Exception):
    """Return a 404 Not Found response."""
    ...

class BadRequest(Exception):
    """Return a 400 Bad Request response."""
    ...

class Forbidden(Exception):
    """Return a 403 Forbidden response."""
    ...
