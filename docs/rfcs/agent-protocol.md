# RFC: Agent Protocol for apx Apps

> **Status:** Draft
> **Author:** Stuart Gano
> **Date:** 2026-03-24
> **Updated:** 2026-03-26

---

## Problem

Building an agent on Databricks Apps requires hand-wiring three distinct layers:

1. **The LLM loop** — calling FMAPI, parsing tool calls, dispatching to handlers, feeding results back, looping until done
2. **The protocol layer** — implementing `/invocations` (MLflow ResponsesAgent format) and `/.well-known/agent.json` (A2A discovery card) correctly
3. **Multi-agent orchestration** — treating other agents as tools, calling their `/invocations` endpoints, composing results

The Genie Workbench is a case study: ~1,800 lines of agent protocol wiring on top of working domain logic. Every agent reimplements the same loop.

---

## Proposal

An `Agent` class that takes plain typed functions as tools and handles everything underneath — the LLM loop, protocol compliance, tool dispatch, and OBO auth.

### Developer experience

```python
# agent.py
from .core import Dependencies
from .core.agent import Agent

Workspace = Dependencies.Workspace

def get_weather(city: str) -> str:
    """Get current weather for a city."""
    import httpx
    return httpx.get(f"https://wttr.in/{city}?format=3").text

def query_genie(question: str, space_id: str, ws: Workspace) -> str:
    """Answer a natural language question using a Genie Space."""
    return ws.genie.ask(space_id=space_id, question=question).answer or ""

agent = Agent(
    model="databricks-meta-llama-3-3-70b-instruct",
    tools=[get_weather, query_genie],
)
```

That's the whole file. `pyproject.toml` provides the name and description:

```toml
[tool.apx.agent]
name = "genie-workbench"
description = "Genie Space quality control platform"
```

### What apx generates

| Endpoint | Purpose |
|----------|---------|
| `POST /invocations` | MLflow ResponsesAgent protocol — runs the LLM loop |
| `GET /.well-known/agent.json` | A2A discovery card (full spec) |
| `GET /health` | Liveness probe |

### What developers do NOT write

- No LLM loop
- No FMAPI integration
- No tool schema definitions (derived from type hints)
- No tool dispatch
- No protocol serialization/deserialization
- No auth bridging (`Workspace` parameters are injected per-request via OBO)
- No agent card JSON

---

## Protocol details

### `/invocations` — MLflow ResponsesAgent format

**Request:**
```json
{
  "input": [
    {"role": "user", "content": "What's the weather in NYC and what does Genie say about sales there?"}
  ],
  "custom_inputs": {}
}
```

**What apx does internally:**
1. Calls FMAPI with the input messages and tool schemas (OpenAI function calling format)
2. If FMAPI returns tool calls → dispatches each to the matching Python function
3. Appends tool results to the message history
4. Loops until FMAPI returns a final message
5. Returns the ResponsesAgent response

**Response:**
```json
{
  "output": [
    {
      "type": "message",
      "role": "assistant",
      "content": [
        {"type": "output_text", "text": "The weather in NYC is 72°F and sunny. Genie reports..."}
      ]
    }
  ]
}
```

### `/.well-known/agent.json` — A2A discovery card

```json
{
  "schemaVersion": "1.0",
  "name": "genie-workbench",
  "description": "Genie Space quality control platform",
  "url": "https://genie-workbench.my-workspace.databricksapps.com",
  "protocolVersion": "0.3.0",
  "capabilities": {
    "a2aVersion": "0.3.0",
    "streaming": false,
    "multiTurn": true
  },
  "provider": {
    "name": "Databricks",
    "url": "https://databricks.com"
  },
  "authSchemes": [
    {"type": "bearer", "name": "Databricks OBO token"}
  ],
  "skills": [
    {
      "id": "get_weather",
      "name": "get_weather",
      "description": "Get current weather for a city.",
      "inputSchema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
      "outputSchema": {"type": "string"}
    },
    {
      "id": "query_genie",
      "name": "query_genie",
      "description": "Answer a natural language question using a Genie Space.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "question": {"type": "string"},
          "space_id": {"type": "string"}
        },
        "required": ["question", "space_id"]
      },
      "outputSchema": {"type": "string"}
    }
  ]
}
```

Note: `Workspace` parameters are excluded from skill schemas — they are OBO-injected per-request and never part of the agent's external interface.

---

## Tool definition

Tools are plain Python functions. Type hints become the schema, docstrings become descriptions.

```python
def tool_name(param: type, ws: Workspace) -> return_type:
    """Tool description."""
    ...
```

Rules:
- Parameters typed as `Dependencies.*` (including `Workspace`) are injected via FastAPI DI and excluded from the tool schema
- All other typed parameters are tool inputs
- Return type determines the output schema — `str` for plain text, `BaseModel` subclass for structured output
- Docstring is required and becomes the tool description in both FMAPI and the A2A card

---

## Multi-agent orchestration

Sub-agents are registered by URL. Each sub-agent is treated as a tool that accepts a `message: str` and returns a `str` — its description comes from its `/.well-known/agent.json`.

```python
agent = Agent(
    model="databricks-meta-llama-3-3-70b-instruct",
    tools=[get_weather],
    sub_agents=[
        "https://genie-agent.my-workspace.databricksapps.com",
    ],
)
```

At startup, apx fetches each sub-agent's agent card to get its name and description. When the LLM calls a sub-agent tool, apx POSTs to that agent's `/invocations` with the message, forwarding the OBO token.

### Topology

```
root_agent (APX app)
├── get_weather (local tool)
└── genie-agent (sub-agent via /invocations)
    └── query_genie (tool inside genie-agent)
```

Each layer is an independently deployable APX app. The root agent composes them without knowing their internals — only their agent cards.

---

## Eval bridge

```python
from apx.agent import app_predict_fn

predict = app_predict_fn("https://genie-workbench.my-workspace.databricksapps.com")
results = mlflow.genai.evaluate(
    data=eval_dataset,
    predict_fn=predict,
    scorers=[correctness_scorer],
)
```

`app_predict_fn` wraps the `/invocations` endpoint in the ResponsesAgent format expected by `mlflow.genai.evaluate()`.

---

## Implementation phases

### Phase 1 (this PR): Protocol foundation
- `Agent(model, tools)` class
- Type hint inspection → tool schemas
- `Workspace` / `Dependencies.*` parameter exclusion
- `/invocations` accepting ResponsesAgent format
- `/.well-known/agent.json` A2A-compliant card
- `/health`

### Phase 2: LLM loop
- FMAPI integration via `ws.serving_endpoints.query()`
- OpenAI-compatible tool call parsing
- Tool dispatch loop
- Result serialization back to ResponsesAgent format

### Phase 3: Sub-agents
- `Agent(sub_agents=[url, ...])`
- Agent card fetching at startup
- Sub-agent dispatch via `/invocations`
- OBO token forwarding

### Phase 4: Eval bridge + streaming
- `app_predict_fn()`
- SSE streaming from `/invocations`
- `apx deploy --agents` (multi-agent deployment)

---

## Design principles

1. **Functions are tools.** No route decorators, no Pydantic models, no `operation_id`. A typed function with a docstring is sufficient.

2. **`Workspace` is the only Databricks-specific concept.** Everything else is plain Python.

3. **Protocol-correct by default.** `/invocations` speaks MLflow ResponsesAgent. `/.well-known/agent.json` speaks A2A. Developers never see these formats.

4. **Composable.** Any APX app can be a sub-agent of another. The root agent only needs a URL.

---

## What this replaces in the Genie Workbench

- 1,757 lines of hand-wired `/invocations` protocol handling (`auto_optimize.py`)
- ~580 lines of hand-written JSON tool schemas (`create_agent_tools.py`)
- ~40 lines of tool dispatch tables
- Separate `auth_bridge.py` / `obo_context()`
- Separate agent card construction
- Separate health check endpoints
