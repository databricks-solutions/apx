from contextlib import asynccontextmanager
from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from .config import conf
from .router import api
from .utils import add_not_found_handler
from .runtime import rt
from .logger import logger


@asynccontextmanager
async def lifespan(app: FastAPI):
    logger.info(
        f"Starting app with configuration:\n{conf.model_dump_json(indent=2)}"
    )
    rt.validate_db()
    rt.initialize_models()
    yield


app = FastAPI(title=f"{conf.app_name}", lifespan=lifespan)
ui = StaticFiles(directory=conf.static_assets_path, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
