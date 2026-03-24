"""Agent protocol addon — A2A discovery, /invocations, MCP tools, eval bridge.

Reads [tool.apx.agent] from pyproject.toml and generates agent protocol
endpoints from the app's existing routes. Routes are tools — no new
abstractions needed.

Configuration (pyproject.toml):

    [tool.apx.agent]
    name = "my-agent"
    description = "What this agent does"
    # tools = ["operationA", "operationB"]  # optional: filter by operation_id
"""

from __future__ import annotations

import json
import logging
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Any, AsyncGenerator, TypeAlias

from fastapi import APIRouter, FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from ._base import LifespanDependency

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Models
# ---------------------------------------------------------------------------


class AgentConfig(BaseModel):
    """Parsed from [tool.apx.agent] in pyproject.toml."""

    name: str
    description: str = ""
    tools: list[str] | None = None  # None = all operation_ids


class AgentTool(BaseModel):
    """A tool derived from a FastAPI route."""

    name: str  # operation_id
    description: str
    method: str  # HTTP method
    path: str  # route path
    input_schema: dict[str, Any] | None = None
    output_schema: dict[str, Any] | None = None
    streaming: bool = False


class AgentCard(BaseModel):
    """A2A discovery card served at /.well-known/agent.json."""

    name: str
    description: str
    url: str = ""
    tools: list[dict[str, Any]]


class AgentContext:
    """Injectable dependency providing agent config and tool registry."""

    def __init__(self, config: AgentConfig, tools: list[AgentTool], card: AgentCard):
        self.config = config
        self.tools = tools
        self.card = card
        self._tool_map: dict[str, AgentTool] = {t.name: t for t in tools}

    def get_tool(self, name: str) -> AgentTool | None:
        return self._tool_map.get(name)


# ---------------------------------------------------------------------------
# Config loading
# ---------------------------------------------------------------------------


def _load_agent_config() -> AgentConfig | None:
    """Read [tool.apx.agent] from pyproject.toml. Returns None if absent."""
    pyproject_path = Path("pyproject.toml")
    if not pyproject_path.exists():
        # Walk up to find it
        for parent in Path.cwd().parents:
            candidate = parent / "pyproject.toml"
            if candidate.exists():
                pyproject_path = candidate
                break
        else:
            return None

    try:
        import tomllib
    except ModuleNotFoundError:
        import tomli as tomllib  # type: ignore[no-redef]

    with open(pyproject_path, "rb") as f:
        data = tomllib.load(f)

    agent_section = data.get("tool", {}).get("apx", {}).get("agent")
    if not agent_section:
        return None

    return AgentConfig(**agent_section)


# ---------------------------------------------------------------------------
# Tool discovery from OpenAPI spec
# ---------------------------------------------------------------------------


def _build_tool_registry(app: FastAPI, config: AgentConfig) -> list[AgentTool]:
    """Extract agent tools from the app's OpenAPI schema.

    Each route with an operation_id becomes a tool. Tool name = operation_id,
    description = route docstring or summary, schemas from request/response models.
    """
    schema = app.openapi()
    tools: list[AgentTool] = []

    for path, methods in schema.get("paths", {}).items():
        for method, details in methods.items():
            if method in ("options", "head"):
                continue

            operation_id = details.get("operationId")
            if not operation_id:
                continue

            # Filter if tools list is specified
            if config.tools and operation_id not in config.tools:
                continue

            # Skip internal endpoints
            if path.startswith("/.well-known") or path in ("/health", "/invocations"):
                continue

            # Extract description
            description = details.get("description") or details.get("summary") or ""

            # Extract input schema from requestBody
            input_schema = None
            request_body = details.get("requestBody", {})
            json_content = request_body.get("content", {}).get("application/json", {})
            if json_content.get("schema"):
                input_schema = _resolve_schema_ref(json_content["schema"], schema)

            # Extract output schema from 200 response
            output_schema = None
            success_response = details.get("responses", {}).get("200", {})
            response_content = success_response.get("content", {}).get("application/json", {})
            if response_content.get("schema"):
                output_schema = _resolve_schema_ref(response_content["schema"], schema)

            # Detect streaming (text/event-stream response)
            streaming = "text/event-stream" in success_response.get("content", {})

            tools.append(AgentTool(
                name=operation_id,
                description=description,
                method=method.upper(),
                path=path,
                input_schema=input_schema,
                output_schema=output_schema,
                streaming=streaming,
            ))

    logger.info(f"Agent '{config.name}': registered {len(tools)} tools from routes")
    return tools


def _resolve_schema_ref(schema: dict, openapi_schema: dict) -> dict:
    """Resolve $ref pointers in OpenAPI schema."""
    ref = schema.get("$ref")
    if not ref:
        return schema

    # $ref format: "#/components/schemas/ModelName"
    parts = ref.lstrip("#/").split("/")
    resolved = openapi_schema
    for part in parts:
        resolved = resolved.get(part, {})
    return resolved


def _build_agent_card(config: AgentConfig, tools: list[AgentTool]) -> AgentCard:
    """Build the A2A discovery card."""
    return AgentCard(
        name=config.name,
        description=config.description,
        tools=[
            {
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
                "outputSchema": t.output_schema,
                "streaming": t.streaming,
            }
            for t in tools
        ],
    )


# ---------------------------------------------------------------------------
# Invocations handler
# ---------------------------------------------------------------------------


class InvocationRequest(BaseModel):
    """Simplified agent protocol request."""

    tool: str
    arguments: dict[str, Any] = {}


async def _handle_invocation(
    request: Request,
    body: InvocationRequest,
) -> Any:
    """Dispatch an agent invocation to the matching route handler.

    Looks up the tool by operation_id, constructs an internal request to
    the corresponding route, and returns the result.
    """
    agent_ctx: AgentContext = request.app.state.agent_context
    tool = agent_ctx.get_tool(body.tool)

    if not tool:
        raise HTTPException(
            status_code=404,
            detail=f"Unknown tool: {body.tool}. Available: {[t.name for t in agent_ctx.tools]}",
        )

    # Forward to the route via the test client (internal dispatch)
    # This preserves all middleware, DI, and auth handling
    from starlette.testclient import TestClient

    # Build the internal request
    if tool.method == "GET":
        response = TestClient(request.app).get(
            tool.path,
            params=body.arguments,
            headers=dict(request.headers),
        )
    else:
        response = TestClient(request.app).request(
            method=tool.method,
            url=tool.path,
            json=body.arguments,
            headers=dict(request.headers),
        )

    if response.status_code >= 400:
        raise HTTPException(status_code=response.status_code, detail=response.text)

    return response.json()


# ---------------------------------------------------------------------------
# LifespanDependency addon
# ---------------------------------------------------------------------------


class _AgentDependency(LifespanDependency):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        config = _load_agent_config()
        if config is None:
            logger.info("No [tool.apx.agent] config found — agent protocol disabled")
            app.state.agent_context = None
            yield
            return

        # Build tool registry from app's OpenAPI spec
        tools = _build_tool_registry(app, config)
        card = _build_agent_card(config, tools)
        app.state.agent_context = AgentContext(config=config, tools=tools, card=card)

        logger.info(
            f"Agent protocol enabled: {config.name} "
            f"({len(tools)} tools, card at /.well-known/agent.json)"
        )
        yield

    @staticmethod
    def __call__(request: Request) -> AgentContext | None:
        return getattr(request.app.state, "agent_context", None)

    def get_routers(self) -> list[APIRouter]:
        agent_router = APIRouter()

        @agent_router.get(
            "/.well-known/agent.json",
            response_model=AgentCard,
            include_in_schema=False,
        )
        async def agent_card(request: Request):
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            return ctx.card

        @agent_router.post(
            "/invocations",
            include_in_schema=False,
        )
        async def invocations(request: Request, body: InvocationRequest):
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            return await _handle_invocation(request, body)

        @agent_router.get(
            "/health",
            include_in_schema=False,
        )
        async def health():
            return {"status": "ok"}

        return [agent_router]


AgentDependency: TypeAlias = Annotated[AgentContext | None, _AgentDependency.depends()]
