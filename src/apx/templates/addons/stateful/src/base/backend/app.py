from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .._metadata import app_name, dist_dir
from .core import (
    add_not_found_handler,
    bootstrap_app,
    create_db_engine,
    initialize_models,
    validate_db,
)
from .router import api


@asynccontextmanager
async def lifespan(app: FastAPI):
    # config and workspace_client are already in app.state (set by bootstrap_app)
    config = app.state.config
    ws = app.state.workspace_client

    engine = create_db_engine(config, ws)
    validate_db(engine, config)
    initialize_models(engine)

    app.state.engine = engine
    yield


app = FastAPI(title=app_name, lifespan=lifespan)
bootstrap_app(app)  # wraps lifespan: config+ws init runs BEFORE the DB lifespan

ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)

add_not_found_handler(app)
