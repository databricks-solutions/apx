from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from .config import conf
from .router import api
from .utils import add_not_found_handler

app = FastAPI(title=f"{conf.app_name}")
ui = StaticFiles(directory=conf.static_assets_path, html=True)

# note the order of includes and mounts!
app.include_router(api)
app.mount("/", ui)


add_not_found_handler(app)
