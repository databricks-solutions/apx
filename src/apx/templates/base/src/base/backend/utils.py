from fastapi import FastAPI, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel
from starlette.exceptions import HTTPException as StarletteHTTPException
from .config import conf
from pathlib import Path
import tomllib


def add_not_found_handler(app: FastAPI):
    async def http_exception_handler(request: Request, exc: StarletteHTTPException):
        if exc.status_code == 404:
            path = request.url.path
            accept = request.headers.get("accept", "")

            is_api = path.startswith(conf.api_prefix)
            is_get_page_nav = request.method == "GET" and "text/html" in accept

            # Heuristic: if the last path segment looks like a file (has a dot), don't SPA-fallback
            looks_like_asset = "." in path.split("/")[-1]

            if (not is_api) and is_get_page_nav and (not looks_like_asset):
                # Let the SPA router handle it
                return FileResponse(conf.static_assets_path / "index.html")
        # Default: return the original HTTP error (JSON 404 for API, etc.)
        return JSONResponse({"detail": exc.detail}, status_code=exc.status_code)

    app.exception_handler(StarletteHTTPException)(http_exception_handler)


class Metadata(BaseModel):
    app_name: str
    app_module: str


def read_metadata() -> Metadata:
    pyproject_path = Path(__file__).parent.parent.parent / "pyproject.toml"
    pyproject = tomllib.loads(pyproject_path.read_text())
    return Metadata(**pyproject["tool"]["apx"]["metadata"])
