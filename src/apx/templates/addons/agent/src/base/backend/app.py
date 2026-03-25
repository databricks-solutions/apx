from .core import create_app
from .router import router
from . import agent_router as agent_router  # noqa: F401 — registers agent example routes

app = create_app(routers=[router])
