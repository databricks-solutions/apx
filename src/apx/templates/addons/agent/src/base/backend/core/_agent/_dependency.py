"""_AgentDependency — LifespanDependency bridge to apx_agent runtime + dev UI."""

from __future__ import annotations

import logging
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager

from fastapi import APIRouter, FastAPI, Request

from apx_agent import AgentContext, BaseAgent, setup_agent
from apx_agent._dev import build_dev_ui_router, inject_create_tool_meta
from apx_agent._mcp import _build_mcp_components
from apx_agent._models import _get_agent_instance, _set_agent_instance

from .._base import LifespanDependency

logger = logging.getLogger(__name__)


def _auto_import_agent_router() -> None:
    """Import agent_router and register the module-level ``agent`` variable."""
    if _get_agent_instance() is not None:
        return
    import importlib
    parts = __name__.split(".")
    if len(parts) >= 4:
        backend_pkg = ".".join(parts[:-3])
        try:
            module = importlib.import_module(f"{backend_pkg}.agent_router")
            candidate = getattr(module, "agent", None)
            if isinstance(candidate, BaseAgent):
                _set_agent_instance(candidate)
        except ImportError:
            pass


class _AgentDependency(LifespanDependency):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        _auto_import_agent_router()
        _agent = _get_agent_instance()

        if _agent is None:
            logger.warning(
                "No agent registered. Set a module-level `agent = LlmAgent(tools=[...])` in agent_router.py."
            )
            app.state.agent_context = None
            yield
            return

        ctx = await setup_agent(app, _agent)
        if ctx is None:
            yield
            return

        # Dev mode: inject create_tool when agent_router.py is editable
        from apx_agent._ui_edit import _find_agent_router_path
        if _find_agent_router_path() is not None:
            inject_create_tool_meta(ctx)

        # Setup MCP
        try:
            from contextlib import nullcontext
            from mcp.server.streamable_http_manager import StreamableHTTPSessionManager
            mcp_server, mcp_transport = _build_mcp_components(ctx, app, ctx.config.api_prefix)
            app.state.mcp_server = mcp_server
            app.state.mcp_transport = mcp_transport
            mcp_http_manager = StreamableHTTPSessionManager(mcp_server, stateless=True)
            app.state.mcp_http_manager = mcp_http_manager
            logger.info("MCP server enabled at /mcp/sse (SSE) and /mcp (stateless HTTP)")
            mcp_lifecycle = mcp_http_manager.run()
        except ImportError:
            app.state.mcp_server = None
            app.state.mcp_transport = None
            app.state.mcp_http_manager = None
            logger.warning("mcp package not installed — /mcp endpoints disabled.")
            from contextlib import nullcontext
            mcp_lifecycle = nullcontext()

        async with mcp_lifecycle:
            yield

    @staticmethod
    def __call__(request: Request) -> AgentContext | None:
        return getattr(request.app.state, "agent_context", None)

    def get_routers(self) -> list[APIRouter]:
        _auto_import_agent_router()
        if _get_agent_instance() is None:
            return []
        return _get_agent_instance().get_tool_routers()

    def get_root_routers(self) -> list[APIRouter]:
        try:
            from ..._metadata import api_prefix as _api_prefix
        except ImportError:
            _api_prefix = "/api"
        return [build_dev_ui_router(api_prefix=_api_prefix)]


AgentDependency = _AgentDependency
