from pathlib import Path

__version__: str

def run_cli(args: list[str]) -> int: ...
def generate_openapi(project_root: Path) -> bool: ...
def get_dotenv_vars() -> dict[str, str]: ...

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
