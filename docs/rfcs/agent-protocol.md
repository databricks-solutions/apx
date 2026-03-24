# RFC: Agent Protocol for apx Apps

> **Status:** Draft
> **Author:** Stuart Gano
> **Date:** 2026-03-24

---

## Problem

Databricks Apps built with apx already have everything an agent needs — typed routes with input/output schemas, OBO auth, health checks, and an OpenAPI spec. But exposing an app as a discoverable, callable agent requires hand-wiring:

1. `/invocations` — the Responses Agent protocol endpoint for MLflow serving
2. `/.well-known/agent.json` — A2A (Agent-to-Agent) discovery card
3. MCP server — exposing routes as MCP tools
4. `app_predict_fn()` — bridge for `mlflow.genai.evaluate()`

Each app that wants agent capabilities reimplements all of these. The Genie Workbench is a case study: ~1,800 lines of agent protocol wiring on top of working domain logic.

Meanwhile, apx routes already declare everything the agent protocol needs — names (`operation_id`), typed inputs (Pydantic models), typed outputs, descriptions (docstrings), and auth requirements (`Dependencies.UserClient`). The gap is purely mechanical.

## Proposal

Add a `[tool.apx.agent]` configuration in `pyproject.toml` that tells apx to generate agent protocol endpoints from existing routes. Zero new application code required.

### Configuration

```toml
[tool.apx.agent]
name = "genie-scorer"
description = "IQ scoring for Genie Spaces"
```

That's it. When present, apx generates:

| Endpoint | Purpose | Generated from |
|----------|---------|----------------|
| `POST /invocations` | Responses Agent protocol | Route schemas + dispatch |
| `GET /.well-known/agent.json` | A2A discovery card | agent config + route metadata |
| `GET /health` | Liveness probe | Always 200 (already common in apps) |
| MCP tool descriptors | Tool integration | Route schemas |

### How it works

Routes are tools. A normal apx route:

```python
@router.post("/scan", response_model=ScanResult, operation_id="scanSpace")
def scan_space(request: ScanRequest, user_ws: Dependencies.UserClient) -> ScanResult:
    """Run IQ scan on a Genie Space."""
    space_data = get_serialized_space(request.space_id)
    return calculate_score(space_data)
```

This route already has:
- **Tool name:** `scanSpace` (from `operation_id`)
- **Tool description:** `"Run IQ scan on a Genie Space."` (from docstring)
- **Input schema:** `ScanRequest` (from Pydantic model)
- **Output schema:** `ScanResult` (from `response_model`)
- **Auth:** OBO via `Dependencies.UserClient`

The agent protocol layer reads the OpenAPI spec and generates:

**Agent card** (`/.well-known/agent.json`):
```json
{
  "name": "genie-scorer",
  "description": "IQ scoring for Genie Spaces",
  "url": "https://genie-scorer.cloud.databricks.com",
  "tools": [
    {
      "name": "scanSpace",
      "description": "Run IQ scan on a Genie Space.",
      "inputSchema": { "$ref": "#/components/schemas/ScanRequest" },
      "outputSchema": { "$ref": "#/components/schemas/ScanResult" }
    }
  ]
}
```

**Invocations endpoint** (`POST /invocations`):
Accepts Responses Agent protocol messages, extracts tool calls, dispatches to the corresponding route handler, returns structured results.

### What developers do NOT write

- No agent card JSON
- No `/invocations` handler
- No tool schema definitions (they're the route schemas)
- No tool dispatch table (routes are the dispatch)
- No auth bridging (routes already use `Dependencies.UserClient`)
- No MCP tool registration

### Controlling which routes are tools

By default, all routes with an `operation_id` become agent tools. To exclude a route:

```python
@router.get("/internal-status", operation_id="internalStatus", include_in_schema=False)
def internal_status():
    ...
```

Or to explicitly opt-in instead of opt-out:

```toml
[tool.apx.agent]
name = "genie-scorer"
description = "IQ scoring for Genie Spaces"
tools = ["scanSpace", "getHistory", "toggleStar"]  # only these operation_ids
```

### SSE streaming routes

Routes that return `StreamingResponse` with `media_type="text/event-stream"` are marked as streaming tools in the agent card. The invocations endpoint proxies the SSE stream.

```python
@router.post("/analyze", operation_id="analyzeSpace")
async def analyze(request: AnalyzeRequest, user_ws: Dependencies.UserClient) -> StreamingResponse:
    """Deep analysis of a Genie Space configuration."""
    async def generate():
        async for event in analyzer.run(request.space_id):
            yield f"data: {json.dumps(event)}\n\n"
    return StreamingResponse(generate(), media_type="text/event-stream")
```

No special handling needed — the framework detects the response type.

### Eval bridge

When agent config is present, apx generates an `app_predict_fn()` that wraps the `/invocations` endpoint for `mlflow.genai.evaluate()`:

```python
from apx.agent import app_predict_fn

predict = app_predict_fn("https://genie-scorer.cloud.databricks.com")
results = mlflow.genai.evaluate(
    data=eval_dataset,
    predict_fn=predict,
    scorers=[correctness_scorer],
)
```

### Multi-agent deployment

For apps that serve multiple logical agents (like the Genie Workbench with scoring, analysis, creation, etc.), each agent can be a separate `APIRouter` with its own agent config:

```toml
[tool.apx.agent]
name = "genie-workbench"
description = "Genie Space quality control platform"

[[tool.apx.agent.sub_agents]]
name = "scorer"
prefix = "/api/spaces"
description = "IQ scoring for Genie Spaces"

[[tool.apx.agent.sub_agents]]
name = "analyzer"
prefix = "/api/analyze"
description = "Deep analysis of Genie Space configurations"
```

Routes under each prefix become tools for that sub-agent. Each sub-agent gets its own agent card at `/.well-known/agent/{name}.json`.

## Implementation

### Phase 1: Agent card generation (Rust CLI)

`apx build` reads `[tool.apx.agent]` from `pyproject.toml`, reads the generated OpenAPI spec, and produces `agent.json`. This is pure metadata — no runtime cost.

### Phase 2: Agent addon (Python)

A new addon (`addons/agent/`) that provides:

```python
class _AgentDependency(LifespanDependency):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        # Load agent config from pyproject.toml
        # Build tool registry from app's OpenAPI spec
        app.state.agent_config = load_agent_config()
        app.state.agent_tools = build_tool_registry(app)
        yield

    @staticmethod
    def __call__(request: Request) -> AgentContext:
        return AgentContext(
            config=request.app.state.agent_config,
            tools=request.app.state.agent_tools,
        )

    def get_routers(self) -> list[APIRouter]:
        agent_router = APIRouter()

        @agent_router.get("/.well-known/agent.json")
        async def agent_card(request: Request):
            return request.app.state.agent_config.card

        @agent_router.post("/invocations")
        async def invocations(request: Request):
            # Parse agent protocol message
            # Dispatch to matching route handler
            # Return structured result
            ...

        @agent_router.get("/health")
        async def health():
            return {"status": "ok"}

        return [agent_router]
```

This follows the exact same pattern as the SQL and Lakebase addons. Auto-registers on import. Contributes routes via `get_routers()`. Lifecycle via `lifespan()`.

### Phase 3: MCP tool generation (Rust)

The existing MCP server in `crates/mcp/` gains a new tool: `agent_tools` — which reads the agent card and exposes the app's tools as MCP resources. This enables Claude to discover and call agent tools during development.

### Phase 4: `apx deploy --agents`

Extension to the deployment flow that can deploy sub-agents as separate Databricks Apps from the same codebase, using route prefix filtering. Optional — single-app deployment always works.

## Design principles

1. **Routes are tools.** No new abstraction for defining agent capabilities. If you can write a FastAPI route, you can write an agent tool.

2. **Configuration, not code.** Agent protocol is enabled by `pyproject.toml`, not by changing application code.

3. **Addon, not framework change.** The agent layer is a `LifespanDependency` addon that composes with existing addons (SQL, Lakebase, etc.). It doesn't modify `create_app()` or the core DI system.

4. **OpenAPI is the source of truth.** Tool schemas, names, descriptions, and dispatch all derive from the OpenAPI spec that apx already generates from routes.

## What this replaces

In the Genie Workbench, adopting this pattern would eliminate:
- 1,757 lines of hand-wired `/invocations` protocol handling (`auto_optimize.py`)
- ~580 lines of hand-written JSON tool schemas (`create_agent_tools.py`)
- ~40 lines of tool dispatch tables
- Separate `auth_bridge.py` / `obo_context()` (routes use `Dependencies.UserClient`)
- Separate agent card construction
- Separate health check endpoints

The workbench routes already exist. Adding `[tool.apx.agent]` to `pyproject.toml` would make them agent-callable with zero code changes.
