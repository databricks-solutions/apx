from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from .._metadata import app_name, dist_dir
from .router import api
from .utils import add_not_found_handler
from .logger import logger
from contextlib import asynccontextmanager


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info(f"Starting app with configuration:\n{app_name}")
    yield


app = FastAPI(title=f"{app_name}", lifespan=lifespan)
ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
