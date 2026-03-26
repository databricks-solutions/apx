"""Agent protocol addon — A2A discovery, /invocations, MCP tools, eval bridge.

Define agent tools as plain typed functions and register them with Agent():

    from .core import Dependencies
    from .core.agent import Agent

    Workspace = Dependencies.Workspace

    def query_genie(question: str, space_id: str, ws: Workspace) -> str:
        \"\"\"Answer a natural language question using a Genie Space.\"\"\"
        return ws.genie.ask(space_id=space_id, question=question).answer or ""

    agent = Agent(tools=[query_genie])

Parameters typed as Dependencies.* are injected by FastAPI and excluded from
the tool schema. All other typed parameters become the tool's input schema,
derived from their type hints. The docstring becomes the tool description.

Requires [tool.apx.agent] in pyproject.toml:

    [tool.apx.agent]
    name = "my-agent"
    description = "What this agent does"
"""

from __future__ import annotations

import inspect
import logging
from collections.abc import Callable
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Any, AsyncGenerator, TypeAlias, get_args, get_origin, get_type_hints

from fastapi import APIRouter, FastAPI, HTTPException, Request, params
from fastapi.responses import JSONResponse
from pydantic import BaseModel, create_model

from ._base import LifespanDependency

logger = logging.getLogger(__name__)

# Module-level Agent instance, set when user calls Agent(tools=[...])
_agent_instance: Agent | None = None


# ---------------------------------------------------------------------------
# Models
# ---------------------------------------------------------------------------


class AgentConfig(BaseModel):
    """Parsed from [tool.apx.agent] in pyproject.toml."""

    name: str
    description: str = ""


class AgentTool(BaseModel):
    """A tool derived from a plain Python function."""

    name: str
    description: str
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
    """Provides agent config and tool registry to route handlers."""

    def __init__(self, config: AgentConfig, tools: list[AgentTool], card: AgentCard):
        self.config = config
        self.tools = tools
        self.card = card
        self._tool_map: dict[str, AgentTool] = {t.name: t for t in tools}

    def get_tool(self, name: str) -> AgentTool | None:
        return self._tool_map.get(name)


# ---------------------------------------------------------------------------
# Function inspection helpers
# ---------------------------------------------------------------------------


def _is_fastapi_dependency(annotation: Any) -> bool:
    """Return True if the annotation is a FastAPI Depends (Dependencies.*)."""
    if get_origin(annotation) is not Annotated:
        return False
    return any(isinstance(arg, params.Depends) for arg in get_args(annotation))


def _inspect_tool_fn(fn: Callable) -> tuple[dict[str, tuple[Any, Any]], list[str]]:
    """Inspect a tool function's signature.

    Returns:
        plain_params: {name: (type, default)} for tool input parameters
        dep_param_names: list of parameter names that are FastAPI dependencies
    """
    try:
        hints = get_type_hints(fn, include_extras=True)
    except Exception:
        hints = {}

    sig = inspect.signature(fn)
    plain_params: dict[str, tuple[Any, Any]] = {}
    dep_param_names: list[str] = []

    for name, param in sig.parameters.items():
        annotation = hints.get(name, Any)
        if _is_fastapi_dependency(annotation):
            dep_param_names.append(name)
        else:
            default = param.default if param.default is not inspect.Parameter.empty else ...
            plain_params[name] = (annotation, default)

    return plain_params, dep_param_names


def _make_input_model(fn: Callable, plain_params: dict[str, tuple[Any, Any]]) -> type[BaseModel] | None:
    """Dynamically create a Pydantic input model from the plain parameters."""
    if not plain_params:
        return None
    fields = {name: (annotation, default) for name, (annotation, default) in plain_params.items()}
    return create_model(f"{fn.__name__}_input", **fields)


def _make_route_handler(
    fn: Callable,
    input_model: type[BaseModel] | None,
    dep_param_names: list[str],
) -> Callable:
    """Create a FastAPI route handler that calls fn with injected dependencies."""
    hints = get_type_hints(fn, include_extras=True)
    dep_annotations = {name: hints[name] for name in dep_param_names if name in hints}

    if input_model is not None:
        # Build a handler: (body: InputModel, dep1: Dep1, ...) -> return_type
        async def handler(body: input_model, **kwargs: Any) -> Any:  # type: ignore[valid-type]
            return fn(**body.model_dump(), **kwargs)

        # Inject dependency annotations into the handler's __annotations__
        # so FastAPI can resolve them
        handler.__annotations__ = {"body": input_model, **dep_annotations, "return": hints.get("return", Any)}
        _patch_handler_signature(handler, input_model, dep_annotations)
    else:
        async def handler(**kwargs: Any) -> Any:
            return fn(**kwargs)

        handler.__annotations__ = {**dep_annotations, "return": hints.get("return", Any)}
        _patch_handler_signature(handler, None, dep_annotations)

    handler.__name__ = fn.__name__
    handler.__doc__ = fn.__doc__
    return handler


def _patch_handler_signature(
    handler: Callable,
    input_model: type[BaseModel] | None,
    dep_annotations: dict[str, Any],
) -> None:
    """Replace handler's inspect.Signature so FastAPI sees the right parameters."""
    parameters: list[inspect.Parameter] = []

    if input_model is not None:
        parameters.append(
            inspect.Parameter("body", inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=input_model)
        )

    for dep_name, dep_annotation in dep_annotations.items():
        parameters.append(
            inspect.Parameter(dep_name, inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=dep_annotation)
        )

    handler.__signature__ = inspect.Signature(parameters)  # type: ignore[attr-defined]


# ---------------------------------------------------------------------------
# Config loading
# ---------------------------------------------------------------------------


def _load_agent_config() -> AgentConfig | None:
    """Read [tool.apx.agent] from pyproject.toml. Returns None if absent."""
    pyproject_path = Path("pyproject.toml")
    if not pyproject_path.exists():
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

    return AgentConfig(**{k: v for k, v in agent_section.items() if k in AgentConfig.model_fields})


# ---------------------------------------------------------------------------
# Schema helpers
# ---------------------------------------------------------------------------


def _schema_for_model(model: type[BaseModel] | None) -> dict[str, Any] | None:
    if model is None:
        return None
    return model.model_json_schema()


def _schema_for_return(fn: Callable) -> dict[str, Any] | None:
    hints = get_type_hints(fn)
    return_type = hints.get("return")
    if return_type is None or return_type is type(None):
        return None
    if isinstance(return_type, type) and issubclass(return_type, BaseModel):
        return return_type.model_json_schema()
    return {"type": "string"}


# ---------------------------------------------------------------------------
# Agent class — the user-facing API
# ---------------------------------------------------------------------------


class Agent:
    """Register plain typed functions as agent tools.

    Example::

        def query_genie(question: str, space_id: str, ws: Dependencies.UserClient) -> str:
            \"\"\"Answer a question using a Genie Space.\"\"\"
            return ws.genie.ask(space_id=space_id, question=question).answer or ""

        agent = Agent(tools=[query_genie])

    Dependencies.* parameters are injected by FastAPI and excluded from the
    tool schema. All other typed parameters become tool inputs.
    """

    def __init__(self, tools: list[Callable]) -> None:
        global _agent_instance
        self._tool_fns = tools
        _agent_instance = self

        # Pre-analyze all functions at construction time
        self._analyzed: list[tuple[Callable, dict, list[str], type[BaseModel] | None]] = []
        for fn in tools:
            plain_params, dep_names = _inspect_tool_fn(fn)
            input_model = _make_input_model(fn, plain_params)
            self._analyzed.append((fn, plain_params, dep_names, input_model))

    def build_router(self) -> APIRouter:
        """Build an APIRouter with a POST route for each tool."""
        router = APIRouter()
        for fn, plain_params, dep_names, input_model in self._analyzed:
            handler = _make_route_handler(fn, input_model, dep_names)
            router.add_api_route(
                f"/tools/{fn.__name__}",
                handler,
                methods=["POST"],
                operation_id=fn.__name__,
                summary=fn.__doc__ or fn.__name__,
                response_model=None,
            )
        return router

    def build_tools(self) -> list[AgentTool]:
        return [
            AgentTool(
                name=fn.__name__,
                description=(fn.__doc__ or "").strip(),
                input_schema=_schema_for_model(input_model),
                output_schema=_schema_for_return(fn),
            )
            for fn, _, _, input_model in self._analyzed
        ]


# ---------------------------------------------------------------------------
# Invocations handler
# ---------------------------------------------------------------------------


class InvocationRequest(BaseModel):
    tool: str
    arguments: dict[str, Any] = {}


async def _handle_invocation(request: Request, body: InvocationRequest) -> Any:
    """Dispatch an agent invocation to the matching tool route via ASGI."""
    agent_ctx: AgentContext = request.app.state.agent_context
    tool = agent_ctx.get_tool(body.tool)

    if not tool:
        raise HTTPException(
            status_code=404,
            detail=f"Unknown tool: {body.tool}. Available: {[t.name for t in agent_ctx.tools]}",
        )

    from httpx import ASGITransport, AsyncClient

    async with AsyncClient(
        transport=ASGITransport(app=request.app),
        base_url="http://internal",
    ) as client:
        response = await client.post(
            f"/tools/{body.tool}",
            json=body.arguments,
            headers={"Authorization": request.headers.get("Authorization", "")},
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

        if _agent_instance is None:
            logger.warning(
                "No Agent() registered. Create one with Agent(tools=[...]) in your agent module."
            )
            app.state.agent_context = None
            yield
            return

        tools = _agent_instance.build_tools()
        card = AgentCard(
            name=config.name,
            description=config.description,
            tools=[
                {
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                    "outputSchema": t.output_schema,
                }
                for t in tools
            ],
        )
        app.state.agent_context = AgentContext(config=config, tools=tools, card=card)
        logger.info(f"Agent protocol enabled: {config.name} ({len(tools)} tools)")
        yield

    @staticmethod
    def __call__(request: Request) -> AgentContext | None:
        return getattr(request.app.state, "agent_context", None)

    def get_routers(self) -> list[APIRouter]:
        agent_router = APIRouter()

        @agent_router.get("/.well-known/agent.json", include_in_schema=False)
        async def agent_card(request: Request) -> AgentCard:
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            return ctx.card

        @agent_router.post("/invocations", include_in_schema=False)
        async def invocations(request: Request, body: InvocationRequest) -> Any:
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            return await _handle_invocation(request, body)

        @agent_router.get("/health", include_in_schema=False)
        async def health() -> dict[str, str]:
            return {"status": "ok"}

        # Tool routes from the registered Agent instance
        if _agent_instance is not None:
            agent_router.include_router(_agent_instance.build_router())

        return [agent_router]


AgentDependency: TypeAlias = Annotated[AgentContext | None, _AgentDependency.depends()]
