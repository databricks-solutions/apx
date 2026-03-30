"""Agent protocol addon — A2A discovery, /invocations, /mcp, /_agent dev UI, eval bridge.

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
    model = "databricks-meta-llama-3-3-70b-instruct"

Endpoints added to the app:

    GET  /.well-known/agent.json   A2A discovery card (name, skills, mcpEndpoint, url)
    POST /invocations               MLflow ResponsesAgent — runs FMAPI tool-calling loop
                                    supports stream=true for SSE token streaming
    GET  /health                    Liveness check
    POST /api/tools/<fn_name>       One FastAPI route per registered tool
    GET  /mcp/sse                   MCP server — SSE transport (connect MCP clients here)
    POST /mcp/messages/             MCP SSE return channel (used by transport, not directly)
    GET  /_agent                    Built-in dev chat UI — stream messages, inspect skills,
                                    copy MCP SSE URL for Claude Desktop / Cursor
"""

from __future__ import annotations

import inspect
import logging
from contextlib import asynccontextmanager
from pathlib import Path
from collections.abc import Callable
from typing import Annotated, Any, AsyncGenerator, Protocol, TypeAlias, get_args, get_origin, get_type_hints

from fastapi import APIRouter, FastAPI, HTTPException, Request, params
from fastapi.responses import HTMLResponse, StreamingResponse
from pydantic import BaseModel, create_model

from ._base import LifespanDependency

logger = logging.getLogger(__name__)


class _ToolFn(Protocol):
    """Minimal protocol for tool functions — carries __name__ and __doc__."""

    __name__: str
    __doc__: str | None

    def __call__(self, *args: Any, **kwargs: Any) -> Any: ...

# Module-level Agent instance, set when user calls Agent(tools=[...])
_agent_instance: "BaseAgent | None" = None


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
    streaming: bool = True
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
    url: str = ""  # populated at request time from request.base_url
    protocolVersion: str = "0.3.0"
    capabilities: A2ACapabilities = A2ACapabilities()
    provider: A2AProvider = A2AProvider()
    authSchemes: list[A2AAuthScheme] = [A2AAuthScheme()]
    skills: list[A2ASkill] = []
    mcpEndpoint: str | None = None  # SSE URL for MCP clients; populated at request time


class AgentContext:
    """Provides agent config, tool registry, and root agent to route handlers."""

    def __init__(
        self,
        config: AgentConfig,
        tools: list[AgentTool],
        card: AgentCard,
        agent: "BaseAgent",
    ):
        self.config = config
        self.tools = tools
        self.card = card
        self.agent = agent
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
# Agent types — user-facing API
# ---------------------------------------------------------------------------


class BaseAgent:
    """Abstract base for all APX agent types.

    Subclass to create custom orchestration patterns, or use the built-in
    ``LlmAgent`` (alias: ``Agent``), ``SequentialAgent``, and ``ParallelAgent``.
    """

    async def run(self, messages: list[Message], request: Request) -> str:
        """Run and return the final text response."""
        raise NotImplementedError

    async def stream(self, messages: list[Message], request: Request) -> AsyncGenerator[str, None]:
        """Yield text chunks as the agent produces them.

        The default implementation runs to completion and yields the result
        as a single chunk. Override for true token streaming.
        """
        yield await self.run(messages, request)

    def get_tool_routers(self) -> list[APIRouter]:
        """Return FastAPI routers for this agent's tool endpoints."""
        return []

    def collect_tools(self) -> list[AgentTool]:
        """Return AgentTool descriptors for all local tools in this agent tree."""
        return []

    async def fetch_remote_tools(self) -> list[AgentTool]:
        """Fetch AgentTool descriptors from remote sub-agents (A2A)."""
        return []


class LlmAgent(BaseAgent):
    """LLM-powered agent with tool calling via FMAPI.

    Typed tool functions are registered at construction time. Parameters typed
    as ``Dependencies.*`` are injected by FastAPI and excluded from the schema;
    all other typed parameters become tool inputs derived from their type hints.

    Example::

        def query_genie(question: str, space_id: str, ws: Dependencies.UserClient) -> str:
            \"\"\"Answer a question using a Genie Space.\"\"\"
            return ws.genie.ask(space_id=space_id, question=question).answer or ""

        agent = LlmAgent(tools=[query_genie])
    """

    def __init__(self, tools: list[_ToolFn], sub_agents: list[str] | None = None) -> None:
        self._tool_fns = tools
        self._sub_agent_urls = sub_agents or []

        # Pre-analyze all functions at construction time
        self._analyzed: list[tuple[_ToolFn, dict[str, Any], list[str], type[BaseModel] | None]] = []
        for fn in tools:
            plain_params, dep_names = _inspect_tool_fn(fn)
            input_model = _make_input_model(fn, plain_params)
            self._analyzed.append((fn, plain_params, dep_names, input_model))

    async def run(self, messages: list[Message], request: Request) -> str:
        return await _run_llm_loop(messages, request, self.collect_tools())

    async def stream(self, messages: list[Message], request: Request) -> AsyncGenerator[str, None]:
        text = await _run_llm_loop(messages, request, self.collect_tools())
        chunk_size = 20
        for i in range(0, len(text), chunk_size):
            yield text[i : i + chunk_size]

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

    def get_tool_routers(self) -> list[APIRouter]:
        return [self.build_router()]

    def collect_tools(self) -> list[AgentTool]:
        return [
            AgentTool(
                name=fn.__name__,
                description=(fn.__doc__ or "").strip(),
                input_schema=_schema_for_model(input_model),
                output_schema=_schema_for_return(fn),
            )
            for fn, _, _, input_model in self._analyzed
        ]

    async def fetch_remote_tools(self) -> list[AgentTool]:
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

                raw_name = card.get("name", url.split("/")[-1])
                tool_name = raw_name.replace("-", "_").replace(" ", "_")
                tools.append(AgentTool(
                    name=tool_name,
                    description=card.get("description", f"Agent at {url}"),
                    input_schema={
                        "type": "object",
                        "properties": {"message": {"type": "string", "description": "Message to send"}},
                        "required": ["message"],
                    },
                    output_schema={"type": "string"},
                    sub_agent_url=url.rstrip("/"),
                ))
                logger.info(f"Registered sub-agent '{tool_name}' from {url}")

        return tools


# Backwards-compatible alias
Agent = LlmAgent


class SequentialAgent(BaseAgent):
    """Runs agents in order, each receiving the previous agent's output as context.

    Example::

        planner = LlmAgent(tools=[search, outline])
        writer  = LlmAgent(tools=[draft, format])
        agent   = SequentialAgent([planner, writer])
    """

    def __init__(self, agents: list[BaseAgent]) -> None:
        if not agents:
            raise ValueError("SequentialAgent requires at least one agent")
        self._agents = agents

    async def run(self, messages: list[Message], request: Request) -> str:
        context = list(messages)
        result = ""
        for sub in self._agents:
            result = await sub.run(context, request)
            context.append(Message(role="assistant", content=result))
        return result

    async def stream(self, messages: list[Message], request: Request) -> AsyncGenerator[str, None]:
        context = list(messages)
        for sub in self._agents[:-1]:
            result = await sub.run(context, request)
            context.append(Message(role="assistant", content=result))
        async for chunk in self._agents[-1].stream(context, request):
            yield chunk

    def get_tool_routers(self) -> list[APIRouter]:
        routers: list[APIRouter] = []
        for sub in self._agents:
            routers.extend(sub.get_tool_routers())
        return routers

    def collect_tools(self) -> list[AgentTool]:
        tools: list[AgentTool] = []
        for sub in self._agents:
            tools.extend(sub.collect_tools())
        return tools

    async def fetch_remote_tools(self) -> list[AgentTool]:
        tools: list[AgentTool] = []
        for sub in self._agents:
            tools.extend(await sub.fetch_remote_tools())
        return tools


class ParallelAgent(BaseAgent):
    """Runs all agents concurrently with the same input and merges their responses.

    Example::

        legal    = LlmAgent(tools=[check_contracts])
        finance  = LlmAgent(tools=[check_budget])
        agent    = ParallelAgent([legal, finance])
    """

    def __init__(self, agents: list[BaseAgent]) -> None:
        if not agents:
            raise ValueError("ParallelAgent requires at least one agent")
        self._agents = agents

    async def run(self, messages: list[Message], request: Request) -> str:
        import asyncio

        results = await asyncio.gather(*[sub.run(messages, request) for sub in self._agents])
        return "\n\n".join(str(r) for r in results)

    async def stream(self, messages: list[Message], request: Request) -> AsyncGenerator[str, None]:
        # Collect all parallel results then yield the merged text as one chunk.
        yield await self.run(messages, request)

    def get_tool_routers(self) -> list[APIRouter]:
        routers: list[APIRouter] = []
        for sub in self._agents:
            routers.extend(sub.get_tool_routers())
        return routers

    def collect_tools(self) -> list[AgentTool]:
        tools: list[AgentTool] = []
        for sub in self._agents:
            tools.extend(sub.collect_tools())
        return tools

    async def fetch_remote_tools(self) -> list[AgentTool]:
        tools: list[AgentTool] = []
        for sub in self._agents:
            tools.extend(await sub.fetch_remote_tools())
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
        # Local tool: dispatch via ASGI to /api/tools/<fn> (api_prefix + /tools/<fn>)
        from ..._metadata import api_prefix

        async with AsyncClient(
            transport=ASGITransport(app=request.app),
            base_url="http://internal",
        ) as client:
            response = await client.post(
                f"{api_prefix}/tools/{fn_name}",
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
    input_messages: list[Message],
    request: Request,
    tools: list[AgentTool] | None = None,
) -> str:
    """Run the FMAPI LLM loop and return the final response text.

    Tool calls are dispatched synchronously before the next FMAPI call.
    Loops until FMAPI returns a final message or the safety cap is hit.

    ``tools`` overrides the global tool list for this call. When ``None``,
    all tools registered on the ``AgentContext`` are used. Pass
    ``self.collect_tools()`` from an ``LlmAgent`` to scope calls to only that
    agent's own tools in a composed hierarchy.
    """
    import json as _json

    from databricks.sdk import WorkspaceClient
    from httpx import AsyncClient

    ctx: AgentContext = request.app.state.agent_context
    ws: WorkspaceClient = request.app.state.workspace_client

    messages = [
        {"role": m.role, "content": m.content, **({"name": m.name} if m.name else {})}
        for m in input_messages
    ]
    effective_tools = tools if tools is not None else ctx.tools
    tool_schemas = _build_fmapi_tool_schemas(effective_tools)
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

    ctx: AgentContext | None = request.app.state.agent_context
    if ctx is None:
        raise HTTPException(status_code=503, detail="Agent protocol not configured")

    if body.stream:
        async def _sse_generator() -> AsyncGenerator[str, None]:
            item_id = "msg_001"
            yield f"event: response.output_item.start\ndata: {_json.dumps({'item_id': item_id})}\n\n"
            full_text = ""
            async for chunk in ctx.agent.stream(body.input, request):
                full_text += chunk
                yield f"event: output_text.delta\ndata: {_json.dumps({'item_id': item_id, 'text': chunk})}\n\n"
            output_item = OutputItem(content=[OutputTextContent(text=full_text)])
            yield f"event: response.output_item.done\ndata: {_json.dumps({'item_id': item_id, 'output': output_item.model_dump()})}\n\n"

        return StreamingResponse(_sse_generator(), media_type="text/event-stream")

    text = await ctx.agent.run(body.input, request)
    return InvocationResponse(
        output=[OutputItem(content=[OutputTextContent(text=text)])]
    )


# ---------------------------------------------------------------------------
# Dev UI
# ---------------------------------------------------------------------------


def _render_agent_ui(ctx: AgentContext | None) -> str:
    """Return a self-contained HTML page for interactively testing the agent."""
    import json as _json

    agent_name = ctx.config.name if ctx else "Agent"
    agent_desc = ctx.config.description if ctx else ""
    skills_json = (
        _json.dumps([{"id": s.id, "name": s.name, "description": s.description} for s in ctx.card.skills])
        if ctx else "[]"
    )
    not_configured = ctx is None
    setup_banner = """
<div id="setup-banner">
  <strong>⚠ Agent not configured</strong><br>
  Add <code>[tool.apx.agent]</code> to <code>pyproject.toml</code> and create
  <code>src/{app}/backend/agent_router.py</code> with an <code>Agent(tools=[...])</code> call,
  then restart the dev server.
</div>""" if not_configured else """
<div id="mcp-info">
  <strong>MCP</strong>
  <code id="mcp-url"></code>
  <button id="copy-btn" onclick="copyMcpUrl()" title="Copy SSE URL">Copy</button>
  <span id="copy-ok" style="display:none;color:#4ade80;margin-left:6px">✓ copied</span>
</div>"""

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{agent_name} — APX Dev UI</title>
<style>
  *, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
         background: #0d0d0d; color: #e8e8e8; height: 100vh;
         display: flex; flex-direction: column; }}
  header {{ padding: 12px 20px; background: #111; border-bottom: 1px solid #2a2a2a;
            display: flex; align-items: center; gap: 12px; flex-shrink: 0; }}
  .badge {{ background: #1e3a5f; color: #60b0ff; font-size: 11px; font-weight: 600;
            padding: 2px 8px; border-radius: 4px; letter-spacing: .5px; text-transform: uppercase; }}
  h1 {{ font-size: 16px; font-weight: 600; color: #fff; }}
  .desc {{ font-size: 13px; color: #888; margin-left: auto; }}
  #setup-banner {{ background: #2a1a00; border: 1px solid #5a3a00; color: #ffb84d;
                   padding: 12px 20px; font-size: 13px; line-height: 1.6; flex-shrink: 0; }}
  #setup-banner code {{ background: #1a1000; padding: 1px 5px; border-radius: 3px;
                        font-family: monospace; font-size: 12px; }}
  #mcp-info {{ background: #0f1a0f; border-bottom: 1px solid #1a2a1a; color: #888;
               padding: 6px 20px; font-size: 12px; display: flex; align-items: center;
               gap: 8px; flex-shrink: 0; }}
  #mcp-info strong {{ color: #4ade80; font-size: 11px; letter-spacing: .5px; text-transform: uppercase; }}
  #mcp-info code {{ background: #0a150a; color: #9cf09c; padding: 2px 8px; border-radius: 4px;
                    font-family: monospace; font-size: 11px; }}
  #copy-btn {{ background: transparent; border: 1px solid #2a3a2a; color: #668866;
               border-radius: 4px; padding: 1px 8px; font-size: 11px; cursor: pointer; }}
  #copy-btn:hover {{ border-color: #4ade80; color: #4ade80; }}
  #chat {{ flex: 1; overflow-y: auto; padding: 20px; display: flex;
           flex-direction: column; gap: 16px; }}
  .msg {{ max-width: 720px; line-height: 1.55; font-size: 14px; }}
  .msg.user {{ align-self: flex-end; background: #1a3a5c; color: #cce4ff;
               padding: 10px 14px; border-radius: 12px 12px 2px 12px; }}
  .msg.assistant {{ align-self: flex-start; color: #ddd; white-space: pre-wrap; }}
  .msg.assistant.streaming::after {{ content: "▋"; animation: blink .7s step-end infinite; }}
  .msg.system {{ align-self: center; font-size: 12px; color: #555; font-style: italic; }}
  @keyframes blink {{ 50% {{ opacity: 0; }} }}
  #tools-panel {{ padding: 0 20px 10px; flex-shrink: 0; }}
  details {{ font-size: 12px; color: #555; cursor: pointer; }}
  details summary {{ color: #666; user-select: none; }}
  .skill {{ display: inline-block; background: #1a1a1a; border: 1px solid #2a2a2a;
            border-radius: 6px; padding: 3px 8px; margin: 4px 4px 0 0;
            font-size: 11px; color: #888; }}
  form {{ display: flex; gap: 8px; padding: 12px 20px;
          background: #111; border-top: 1px solid #2a2a2a; flex-shrink: 0; }}
  textarea {{ flex: 1; background: #1a1a1a; border: 1px solid #333; color: #e8e8e8;
              border-radius: 8px; padding: 10px 14px; font-size: 14px; resize: none;
              font-family: inherit; line-height: 1.4; outline: none; max-height: 160px; }}
  textarea:focus {{ border-color: #3a7bd5; }}
  button {{ background: #2563eb; color: #fff; border: none; border-radius: 8px;
            padding: 10px 18px; font-size: 14px; cursor: pointer; align-self: flex-end;
            white-space: nowrap; font-weight: 500; transition: background .15s; }}
  button:hover {{ background: #1d4ed8; }}
  button:disabled {{ background: #1a3060; color: #666; cursor: not-allowed; }}
</style>
</head>
<body>
<header>
  <span class="badge">APX dev</span>
  <h1>{agent_name}</h1>
  <span class="desc">{agent_desc}</span>
</header>
{setup_banner}
<div id="chat">
  <div class="msg system">
    Chat with <strong>{agent_name}</strong> below.
    Conversations are sent as full history each time (stateless agent).
  </div>
</div>

<div id="tools-panel">
  <details id="skills-details">
    <summary>Skills</summary>
    <div id="skills-list" style="margin-top:6px"></div>
  </details>
</div>

<form id="form" autocomplete="off">
  <textarea id="input" rows="1" placeholder="Type a message…" required></textarea>
  <button id="send-btn" type="submit">Send</button>
</form>

<script>
const SKILLS = {skills_json};
const chat = document.getElementById('chat');
const form = document.getElementById('form');
const input = document.getElementById('input');
const sendBtn = document.getElementById('send-btn');
const skillsList = document.getElementById('skills-list');
const skillsDetails = document.getElementById('skills-details');

// Render skills
if (SKILLS.length) {{
  SKILLS.forEach(s => {{
    const el = document.createElement('span');
    el.className = 'skill';
    el.title = s.description;
    el.textContent = s.name;
    skillsList.appendChild(el);
  }});
}} else {{
  skillsDetails.style.display = 'none';
}}

// Conversation history (MLflow ResponsesAgent format)
const history = [];

function addMsg(role, text, streaming) {{
  const div = document.createElement('div');
  div.className = `msg ${{role}}${{streaming ? ' streaming' : ''}}`;
  div.textContent = text;
  chat.appendChild(div);
  chat.scrollTop = chat.scrollHeight;
  return div;
}}

// Auto-grow textarea
input.addEventListener('input', () => {{
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 160) + 'px';
}});

// Submit on Enter (Shift+Enter = newline)
input.addEventListener('keydown', e => {{
  if (e.key === 'Enter' && !e.shiftKey) {{ e.preventDefault(); form.requestSubmit(); }}
}});

form.addEventListener('submit', async e => {{
  e.preventDefault();
  const text = input.value.trim();
  if (!text) return;

  input.value = '';
  input.style.height = 'auto';
  sendBtn.disabled = true;

  addMsg('user', text);
  history.push({{ role: 'user', content: text }});

  const assistantDiv = addMsg('assistant', '', true);
  let full = '';

  try {{
    const res = await fetch('/invocations', {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ input: history, stream: true }}),
    }});

    if (!res.ok) {{
      const err = await res.text();
      throw new Error(`${{res.status}} ${{err}}`);
    }}

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = '';

    while (true) {{
      const {{ done, value }} = await reader.read();
      if (done) break;
      buf += decoder.decode(value, {{ stream: true }});
      const lines = buf.split('\\n');
      buf = lines.pop(); // hold incomplete line

      let eventType = '';
      for (const line of lines) {{
        if (line.startsWith('event: ')) {{ eventType = line.slice(7).trim(); }}
        else if (line.startsWith('data: ')) {{
          try {{
            const payload = JSON.parse(line.slice(6));
            if (eventType === 'output_text.delta' && payload.text) {{
              full += payload.text;
              assistantDiv.textContent = full;
              chat.scrollTop = chat.scrollHeight;
            }}
          }} catch {{}}
        }}
      }}
    }}
  }} catch (err) {{
    full = `Error: ${{err.message}}`;
    assistantDiv.textContent = full;
  }}

  assistantDiv.classList.remove('streaming');
  history.push({{ role: 'assistant', content: full }});
  sendBtn.disabled = false;
  input.focus();
}});

// MCP URL (computed client-side so it works on any port)
const mcpUrlEl = document.getElementById('mcp-url');
if (mcpUrlEl) mcpUrlEl.textContent = `${{window.location.origin}}/mcp/sse`;

function copyMcpUrl() {{
  const url = `${{window.location.origin}}/mcp/sse`;
  navigator.clipboard.writeText(url).then(() => {{
    const ok = document.getElementById('copy-ok');
    ok.style.display = 'inline';
    setTimeout(() => {{ ok.style.display = 'none'; }}, 2000);
  }});
}}

input.focus();
</script>
</body>
</html>"""


# ---------------------------------------------------------------------------
# MCP server (SSE transport)
# ---------------------------------------------------------------------------


def _build_mcp_components(ctx: AgentContext, app: FastAPI) -> tuple[Any, Any]:
    """Build an MCP Server + SseServerTransport from the agent's tool registry.

    Returns (server, sse_transport) to be stored on app.state.
    Tool calls are dispatched via ASGI to the existing /api/tools/<name> routes
    so they share the same FastAPI dependency injection (auth, workspace client, etc.).
    """
    from mcp.server import Server
    from mcp.server.sse import SseServerTransport
    import mcp.types as mcp_types

    server: Any = Server(ctx.config.name)
    sse = SseServerTransport("/mcp/messages/")

    @server.list_tools()
    async def _list_tools() -> list[Any]:
        return [
            mcp_types.Tool(
                name=t.name,
                description=t.description or "",
                inputSchema=t.input_schema or {"type": "object", "properties": {}},
            )
            for t in ctx.tools
        ]

    @server.call_tool()
    async def _call_tool(name: str, arguments: dict[str, Any] | None) -> list[Any]:
        from httpx import ASGITransport, AsyncClient

        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://internal",
        ) as client:
            response = await client.post(f"/api/tools/{name}", json=arguments or {})

        if response.status_code >= 400:
            text = f"Tool error ({response.status_code}): {response.text}"
        else:
            result = response.json()
            text = result if isinstance(result, str) else str(result)

        return [mcp_types.TextContent(type="text", text=text)]

    return server, sse


# ---------------------------------------------------------------------------
# Auto-discovery
# ---------------------------------------------------------------------------


def _auto_import_agent_router() -> None:
    """Import agent_router and register the module-level ``agent`` variable.

    Convention: ``agent_router.py`` lives one level up from ``core/`` and
    exposes a module-level ``agent`` variable that is a ``BaseAgent`` instance:

        {pkg}.backend.core.agent   ← this module (__name__)
        {pkg}.backend.agent_router ← discovered here; its ``agent`` is registered

    Sub-agents constructed inside ``agent_router.py`` do NOT auto-register —
    only the top-level ``agent`` assignment does. This avoids sub-agents in a
    ``SequentialAgent`` or ``ParallelAgent`` accidentally overwriting the root.
    """
    global _agent_instance
    if _agent_instance is not None:
        return
    import importlib

    parts = __name__.split(".")
    if len(parts) >= 3:
        backend_pkg = ".".join(parts[:-2])
        try:
            module = importlib.import_module(f"{backend_pkg}.agent_router")
            candidate = getattr(module, "agent", None)
            if isinstance(candidate, BaseAgent):
                _agent_instance = candidate
        except ImportError:
            pass  # No agent_router.py — agent stays disabled


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
                "No agent registered. Set a module-level `agent = LlmAgent(tools=[...])` (or SequentialAgent/ParallelAgent) in agent_router.py."
            )
            app.state.agent_context = None
            yield
            return

        tools = _agent_instance.collect_tools()
        tools += await _agent_instance.fetch_remote_tools()
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
        ctx = AgentContext(config=config, tools=tools, card=card, agent=_agent_instance)
        app.state.agent_context = ctx
        logger.info(f"Agent protocol enabled: {config.name} ({len(tools)} tools)")

        try:
            mcp_server, mcp_transport = _build_mcp_components(ctx, app)
            app.state.mcp_server = mcp_server
            app.state.mcp_transport = mcp_transport
            logger.info("MCP server enabled at /mcp/sse")
        except ImportError:
            app.state.mcp_server = None
            app.state.mcp_transport = None
            logger.warning("mcp package not installed — /mcp endpoints disabled. Add mcp>=1.0.0 to dependencies.")

        yield

    @staticmethod
    def __call__(request: Request) -> AgentContext | None:
        return getattr(request.app.state, "agent_context", None)

    def get_routers(self) -> list[APIRouter]:
        """Tool routes — mounted under the api prefix (e.g. /api/tools/...)."""
        _auto_import_agent_router()
        if _agent_instance is None:
            return []
        return _agent_instance.get_tool_routers()

    def get_root_routers(self) -> list[APIRouter]:
        """Protocol routes — mounted at app root: /.well-known, /invocations, /health."""
        agent_router = APIRouter()

        @agent_router.get("/.well-known/agent.json", include_in_schema=False)
        async def agent_card(request: Request) -> AgentCard:
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            base = str(request.base_url).rstrip("/")
            mcp_available = getattr(request.app.state, "mcp_server", None) is not None
            return ctx.card.model_copy(update={
                "url": base,
                "mcpEndpoint": f"{base}/mcp/sse" if mcp_available else None,
            })

        @agent_router.post("/invocations", include_in_schema=False)
        async def invocations(request: Request, body: InvocationRequest) -> Any:
            ctx: AgentContext | None = request.app.state.agent_context
            if ctx is None:
                raise HTTPException(status_code=404, detail="Agent protocol not configured")
            return await _handle_invocation(request, body)

        @agent_router.get("/health", include_in_schema=False)
        async def health() -> dict[str, str]:
            return {"status": "ok"}

        @agent_router.get("/mcp/sse", include_in_schema=False)
        async def mcp_sse(request: Request) -> Any:
            """MCP SSE transport — connect MCP clients here."""
            mcp_server = getattr(request.app.state, "mcp_server", None)
            mcp_transport = getattr(request.app.state, "mcp_transport", None)
            if mcp_server is None or mcp_transport is None:
                raise HTTPException(status_code=503, detail="MCP server not available")
            async with mcp_transport.connect_sse(
                request.scope, request.receive, request._send
            ) as streams:
                await mcp_server.run(
                    streams[0], streams[1],
                    mcp_server.create_initialization_options(),
                )

        @agent_router.post("/mcp/messages/", include_in_schema=False)
        async def mcp_messages(request: Request) -> Any:
            """MCP SSE transport — message channel (used by the SSE transport)."""
            mcp_transport = getattr(request.app.state, "mcp_transport", None)
            if mcp_transport is None:
                raise HTTPException(status_code=503, detail="MCP server not available")
            await mcp_transport.handle_post_message(
                request.scope, request.receive, request._send
            )

        @agent_router.get("/_agent", include_in_schema=False)
        async def agent_dev_ui(request: Request) -> HTMLResponse:
            ctx: AgentContext | None = request.app.state.agent_context
            return HTMLResponse(_render_agent_ui(ctx))

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
