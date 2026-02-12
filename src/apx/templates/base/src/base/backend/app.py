from .core import create_app
from .router import router

app = create_app(routers=[router])
