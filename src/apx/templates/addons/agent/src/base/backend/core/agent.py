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
from contextlib import asynccontextmanager
from pathlib import Path
from collections.abc import Callable
from typing import Annotated, Any, AsyncGenerator, Protocol, TypeAlias, get_args, get_origin, get_type_hints

from fastapi import APIRouter, FastAPI, HTTPException, Request, params
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel, create_model

from ._base import LifespanDependency

logger = logging.getLogger(__name__)


class _ToolFn(Protocol):
    """Minimal protocol for tool functions — carries __name__ and __doc__."""

    __name__: str
    __doc__: str | None

    def __call__(self, *args: Any, **kwargs: Any) -> Any: ...

# Module-level Agent instance, set when user calls Agent(tools=[...])
_agent_instance: Agent | None = None


# ---------------------------------------------------------------------------
# Models
# ---------------------------------------------------------------------------


class AgentConfig(BaseModel):
    """Parsed from [tool.apx.agent] in pyproject.toml."""

    name: str
    description: str = ""
    model: str = "databricks-meta-llama-3-3-70b-instruct"


class AgentTool(BaseModel):
    """A tool derived from a plain Python function or a remote sub-agent."""

    name: str
    description: str
    input_schema: dict[str, Any] | None = None
    output_schema: dict[str, Any] | None = None
    sub_agent_url: str | None = None  # set for sub-agent tools, None for local tools


# ---------------------------------------------------------------------------
# ResponsesAgent protocol models (MLflow/Databricks)
# ---------------------------------------------------------------------------


class Message(BaseModel):
    """A single message in the conversation history."""

    role: str  # "user" | "assistant" | "system" | "tool"
    content: str
    id: str | None = None
    name: str | None = None
    tool_call_id: str | None = None


class InvocationRequest(BaseModel):
    """MLflow ResponsesAgent /invocations request format."""

    input: list[Message]
    custom_inputs: dict[str, Any] = {}
    stream: bool = False


class OutputTextContent(BaseModel):
    type: str = "output_text"
    text: str


class OutputItem(BaseModel):
    type: str = "message"
    role: str = "assistant"
    id: str | None = None
    status: str = "completed"
    content: list[OutputTextContent]


class InvocationResponse(BaseModel):
    """MLflow ResponsesAgent /invocations response format."""

    output: list[OutputItem]
    custom_outputs: dict[str, Any] = {}


# ---------------------------------------------------------------------------
# A2A discovery card models
# ---------------------------------------------------------------------------


class A2ACapabilities(BaseModel):
    a2aVersion: str = "0.3.0"
    streaming: bool = False
    multiTurn: bool = True


class A2AProvider(BaseModel):
    name: str = "Databricks"
    url: str = "https://databricks.com"


class A2AAuthScheme(BaseModel):
    type: str = "bearer"
    name: str = "Databricks OBO token"


class A2ASkill(BaseModel):
    id: str
    name: str
    description: str
    inputSchema: dict[str, Any] | None = None
    outputSchema: dict[str, Any] | None = None


class AgentCard(BaseModel):
    """A2A discovery card served at /.well-known/agent.json."""

    schemaVersion: str = "1.0"
    name: str
    description: str
    url: str = ""
    protocolVersion: str = "0.3.0"
    capabilities: A2ACapabilities = A2ACapabilities()
    provider: A2AProvider = A2AProvider()
    authSchemes: list[A2AAuthScheme] = [A2AAuthScheme()]
    skills: list[A2ASkill] = []


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


def _inspect_tool_fn(fn: _ToolFn) -> tuple[dict[str, tuple[Any, Any]], list[str]]:
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


def _make_input_model(fn: _ToolFn, plain_params: dict[str, tuple[Any, Any]]) -> type[BaseModel] | None:
    """Dynamically create a Pydantic input model from the plain parameters."""
    if not plain_params:
        return None
    fields = {name: (annotation, default) for name, (annotation, default) in plain_params.items()}
    return create_model(f"{fn.__name__}_input", **fields)  # type: ignore


def _make_route_handler(
    fn: _ToolFn,
    input_model: type[BaseModel] | None,
    dep_param_names: list[str],
) -> Any:
    """Create a FastAPI route handler that calls fn with injected dependencies."""
    hints = get_type_hints(fn, include_extras=True)
    dep_annotations = {name: hints[name] for name in dep_param_names if name in hints}

    if input_model is not None:
        # Build a handler: (body: InputModel, dep1: Dep1, ...) -> return_type
        async def handler_with_body(body: Any, **kwargs: Any) -> Any:
            return fn(**body.model_dump(), **kwargs)

        handler_with_body.__annotations__ = {
            "body": input_model,
            **dep_annotations,
            "return": hints.get("return", Any),
        }
        _patch_handler_signature(handler_with_body, input_model, dep_annotations)
        handler_with_body.__name__ = fn.__name__  # type: ignore[method-assign]
        handler_with_body.__doc__ = fn.__doc__  # type: ignore[method-assign]
        return handler_with_body
    else:
        async def handler_no_body(**kwargs: Any) -> Any:
            return fn(**kwargs)

        handler_no_body.__annotations__ = {**dep_annotations, "return": hints.get("return", Any)}
        _patch_handler_signature(handler_no_body, None, dep_annotations)
        handler_no_body.__name__ = fn.__name__  # type: ignore[method-assign]
        handler_no_body.__doc__ = fn.__doc__  # type: ignore[method-assign]
        return handler_no_body


def _patch_handler_signature(
    handler: Any,
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

    import tomllib

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


def _schema_for_return(fn: _ToolFn) -> dict[str, Any] | None:
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

    def __init__(self, tools: list[_ToolFn], sub_agents: list[str] | None = None) -> None:
        global _agent_instance
        self._tool_fns = tools
        self._sub_agent_urls = sub_agents or []
        _agent_instance = self

        # Pre-analyze all functions at construction time
        self._analyzed: list[tuple[_ToolFn, dict[str, Any], list[str], type[BaseModel] | None]] = []
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

    def build_local_tools(self) -> list[AgentTool]:
        return [
            AgentTool(
                name=fn.__name__,
                description=(fn.__doc__ or "").strip(),
                input_schema=_schema_for_model(input_model),
                output_schema=_schema_for_return(fn),
            )
            for fn, _, _, input_model in self._analyzed
        ]

    async def fetch_sub_agent_tools(self) -> list[AgentTool]:
        """Fetch agent cards from sub-agent URLs and build tools from them."""
        from httpx import AsyncClient

        tools: list[AgentTool] = []
        async with AsyncClient(timeout=10.0) as client:
            for url in self._sub_agent_urls:
                card_url = f"{url.rstrip('/')}/.well-known/agent.json"
                try:
                    response = await client.get(card_url)
                    response.raise_for_status()
                    card = response.json()
                except Exception as e:
                    logger.warning(f"Failed to fetch agent card from {card_url}: {e}")
                    continue

                # Sanitize the agent name to a valid Python identifier
                raw_name = card.get("name", url.split("/")[-1])
                tool_name = raw_name.replace("-", "_").replace(" ", "_")

                tools.append(AgentTool(
                    name=tool_name,
                    description=card.get("description", f"Agent at {url}"),
                    input_schema={
                        "type": "object",
                        "properties": {
                            "message": {"type": "string", "description": "The message to send to the agent"},
                        },
                        "required": ["message"],
                    },
                    output_schema={"type": "string"},
                    sub_agent_url=url.rstrip("/"),
                ))
                logger.info(f"Registered sub-agent '{tool_name}' from {url}")

        return tools


# ---------------------------------------------------------------------------
# LLM loop helpers
# ---------------------------------------------------------------------------


def _build_fmapi_tool_schemas(tools: list[AgentTool]) -> list[dict[str, Any]]:
    """Convert AgentTools to OpenAI function calling format for FMAPI."""
    return [
        {
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema or {"type": "object", "properties": {}},
            },
        }
        for t in tools
    ]


async def _dispatch_tool_call(
    request: Request,
    tool_call: dict[str, Any],
    ctx: AgentContext,
) -> Any:
    """Dispatch a single FMAPI tool call — local via ASGI, sub-agent via HTTP."""
    import json as _json

    from httpx import ASGITransport, AsyncClient

    fn_name = tool_call["function"]["name"]
    try:
        arguments = _json.loads(tool_call["function"].get("arguments", "{}"))
    except Exception:
        arguments = {}

    tool = ctx.get_tool(fn_name)
    obo_header = {"Authorization": request.headers.get("Authorization", "")}

    if tool and tool.sub_agent_url:
        # Sub-agent: POST to its /invocations with the message
        message = arguments.get("message", _json.dumps(arguments))
        async with AsyncClient(timeout=60.0) as client:
            response = await client.post(
                f"{tool.sub_agent_url}/invocations",
                json={"input": [{"role": "user", "content": message}]},
                headers=obo_header,
            )
        if response.status_code >= 400:
            return f"Sub-agent error ({response.status_code}): {response.text}"
        data = response.json()
        # Extract text from ResponsesAgent output format
        try:
            return data["output"][0]["content"][0]["text"]
        except (KeyError, IndexError):
            return str(data)
    else:
        # Local tool: dispatch via ASGI
        async with AsyncClient(
            transport=ASGITransport(app=request.app),
            base_url="http://internal",
        ) as client:
            response = await client.post(
                f"/tools/{fn_name}",
                json=arguments,
                headers=obo_header,
            )
        if response.status_code >= 400:
            return f"Tool error ({response.status_code}): {response.text}"
        result = response.json()
        return result if isinstance(result, str) else str(result)


# ---------------------------------------------------------------------------
# Invocations handler
# ---------------------------------------------------------------------------


async def _run_llm_loop(
    request: Request,
    body: InvocationRequest,
) -> str:
    """Run the FMAPI LLM loop and return the final response text.

    Tool calls are dispatched synchronously before the next FMAPI call.
    Loops until FMAPI returns a final message or the safety cap is hit.
    """
    import json as _json

    from databricks.sdk import WorkspaceClient
    from httpx import AsyncClient

    ctx: AgentContext = request.app.state.agent_context
    ws: WorkspaceClient = request.app.state.workspace_client

    messages = [
        {"role": m.role, "content": m.content, **({"name": m.name} if m.name else {})}
        for m in body.input
    ]
    tool_schemas = _build_fmapi_tool_schemas(ctx.tools)
    auth_headers = ws.config.authenticate()
    fmapi_url = f"{ws.config.host.rstrip('/')}/serving-endpoints/{ctx.config.model}/invocations"

    async with AsyncClient() as client:
        for _ in range(10):  # max iterations as a safety cap
            response = await client.post(
                fmapi_url,
                json={"messages": messages, "tools": tool_schemas},
                headers=auth_headers,
                timeout=60.0,
            )
            response.raise_for_status()
            data = response.json()

            choice = data["choices"][0]
            assistant_msg = choice["message"]
            finish_reason = choice.get("finish_reason") or choice.get("finishReason")
            messages.append(assistant_msg)

            if finish_reason == "tool_calls":
                for tool_call in assistant_msg.get("tool_calls", []):
                    result = await _dispatch_tool_call(request, tool_call, ctx)
                    messages.append({
                        "role": "tool",
                        "tool_call_id": tool_call["id"],
                        "content": result if isinstance(result, str) else _json.dumps(result),
                    })
            else:
                return assistant_msg.get("content") or ""

    # Safety cap hit
    return next(
        (m.get("content", "") for m in reversed(messages) if m.get("role") == "assistant"),
        "",
    )


async def _handle_invocation(
    request: Request,
    body: InvocationRequest,
) -> InvocationResponse | StreamingResponse:
    """Handle /invocations — returns JSON or SSE depending on body.stream."""
    import json as _json

    if body.stream:
        async def _sse_generator() -> AsyncGenerator[str, None]:
            item_id = "msg_001"
            yield f"event: response.output_item.start\ndata: {_json.dumps({'item_id': item_id})}\n\n"
            text = await _run_llm_loop(request, body)
            # Stream the text in chunks
            chunk_size = 20
            for i in range(0, len(text), chunk_size):
                chunk = text[i:i + chunk_size]
                yield f"event: output_text.delta\ndata: {_json.dumps({'item_id': item_id, 'text': chunk})}\n\n"
            output_item = OutputItem(content=[OutputTextContent(text=text)])
            yield f"event: response.output_item.done\ndata: {_json.dumps({'item_id': item_id, 'output': output_item.model_dump()})}\n\n"

        return StreamingResponse(_sse_generator(), media_type="text/event-stream")

    text = await _run_llm_loop(request, body)
    return InvocationResponse(
        output=[OutputItem(content=[OutputTextContent(text=text)])]
    )


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

        tools = _agent_instance.build_local_tools()
        tools += await _agent_instance.fetch_sub_agent_tools()
        card = AgentCard(
            name=config.name,
            description=config.description,
            skills=[
                A2ASkill(
                    id=t.name,
                    name=t.name,
                    description=t.description,
                    inputSchema=t.input_schema,
                    outputSchema=t.output_schema,
                )
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
        """Tool routes — mounted under the api prefix (e.g. /api/tools/...)."""
        if _agent_instance is None:
            return []
        return [_agent_instance.build_router()]

    def get_root_routers(self) -> list[APIRouter]:
        """Protocol routes — mounted at app root: /.well-known, /invocations, /health."""
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

        return [agent_router]


AgentDependency: TypeAlias = Annotated[AgentContext | None, _AgentDependency.depends()]


# ---------------------------------------------------------------------------
# Eval bridge
# ---------------------------------------------------------------------------


def app_predict_fn(url: str) -> Callable[[dict[str, Any]], str]:
    """Return a predict function for mlflow.genai.evaluate().

    Example::

        from apx.agent import app_predict_fn

        predict = app_predict_fn("https://my-agent.my-workspace.databricksapps.com")
        results = mlflow.genai.evaluate(
            data=eval_dataset,
            predict_fn=predict,
            scorers=[correctness_scorer],
        )

    The predict function accepts a dict with a "messages" key (list of message
    dicts) or a plain string, posts to the agent's /invocations endpoint, and
    returns the response text.
    """
    import httpx

    base = url.rstrip("/")

    def predict(inputs: dict[str, Any]) -> str:
        if isinstance(inputs, str):
            messages = [{"role": "user", "content": inputs}]
        else:
            messages = inputs.get("messages") or [
                {"role": "user", "content": str(inputs.get("input", inputs))}
            ]

        response = httpx.post(
            f"{base}/invocations",
            json={"input": messages},
            timeout=120.0,
        )
        response.raise_for_status()
        data = response.json()
        try:
            return data["output"][0]["content"][0]["text"]
        except (KeyError, IndexError):
            return str(data)

    return predict
