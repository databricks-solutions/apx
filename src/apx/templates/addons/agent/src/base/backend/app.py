from .core import create_app
from .router import router
from . import agent_router as agent_router  # noqa: F401 — registers Agent instance before create_app

app = create_app(routers=[router])
