from contextlib import asynccontextmanager
from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from .._metadata import app_name, dist_dir
from .router import api
from .utils import add_not_found_handler
from ..runtime import rt


@asynccontextmanager
async def lifespan(app: FastAPI):
    rt.validate_db()
    rt.initialize_models()
    yield


app = FastAPI(title=f"{app_name}", lifespan=lifespan)
ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
