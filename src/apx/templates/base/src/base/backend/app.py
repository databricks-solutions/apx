from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .._metadata import app_name, dist_dir
from .core import add_not_found_handler, bootstrap_app
from .router import api

app = FastAPI(title=app_name)
bootstrap_app(app)

ui = StaticFiles(directory=dist_dir, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)

add_not_found_handler(app)
