from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .._metadata import app_name, dist_dir
from .router import api
from .dependencies import get_runtime
from .utils import add_not_found_handler


@asynccontextmanager
async def lifespan(app: FastAPI):
    runtime = get_runtime()
    runtime.validate_db()
    runtime.initialize_models()
    yield


app = FastAPI(title=f"{app_name}", lifespan=lifespan)
ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
