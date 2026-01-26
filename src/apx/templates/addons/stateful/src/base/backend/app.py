from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .._metadata import app_name, dist_dir
from .router import api
from .dependencies import get_runtime
from .utils import add_not_found_handler
from .logger import logger


@asynccontextmanager
async def lifespan(app: FastAPI):
    try:
        runtime = get_runtime()
        runtime.validate_db()
        runtime.initialize_models()
    except Exception as e:
        logger.error(f"Failed to initialize application: {e}", exc_info=True)
        raise
    yield


app = FastAPI(title=f"{app_name}", lifespan=lifespan)
ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
