from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .api import router
from .profiling import install_profiling

app = FastAPI(title="APX Bench App")
app.include_router(router, prefix="/api")

# Install ASGI profiling middleware when APX_BENCH_PROFILE=1.
# Uses app.add_middleware() so the FastAPI instance stays discoverable.
install_profiling(app)

_static_dir = Path(__file__).parent / "static"
app.mount("/", StaticFiles(directory=str(_static_dir), html=True), name="static")
