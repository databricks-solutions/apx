"""Standalone tests for the agent addon's core Python logic.

No APX wheel build required — runs against the template source directly.

Usage:
    python3 scripts/dev/test_agent.py

Requirements: fastapi and pydantic must be importable (standard dev env).
"""

import importlib.util
import inspect
import json
import pathlib
import sys
import types
from typing import Annotated, Any

# ---------------------------------------------------------------------------
# Minimal stubs to satisfy agent.py's APX relative imports
# ---------------------------------------------------------------------------

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
CORE_DIR = (
    REPO_ROOT
    / "src/apx/templates/addons/agent/src/base/backend/core"
)

_pkg_name = "core"
_pkg_mod = types.ModuleType(_pkg_name)
_pkg_mod.__path__ = [str(CORE_DIR)]
_pkg_mod.__package__ = _pkg_name
sys.modules[_pkg_name] = _pkg_mod

_base_mod = types.ModuleType(f"{_pkg_name}._base")


class _LifespanDependency:
    @classmethod
    def depends(cls):
        from fastapi import Depends
        return Depends(cls())


_base_mod.LifespanDependency = _LifespanDependency
sys.modules[f"{_pkg_name}._base"] = _base_mod

AGENT_PY = CORE_DIR / "agent.py"
spec = importlib.util.spec_from_file_location(f"{_pkg_name}.agent", AGENT_PY)
agent_mod = importlib.util.module_from_spec(spec)
agent_mod.__package__ = _pkg_name
sys.modules[f"{_pkg_name}.agent"] = agent_mod
spec.loader.exec_module(agent_mod)

# Pull the names we test
_inspect_tool_fn = agent_mod._inspect_tool_fn
_make_input_model = agent_mod._make_input_model
_build_fmapi_tool_schemas = agent_mod._build_fmapi_tool_schemas
Agent = agent_mod.Agent
AgentCard = agent_mod.AgentCard
AgentConfig = agent_mod.AgentConfig
AgentContext = agent_mod.AgentContext
AgentTool = agent_mod.AgentTool
InvocationRequest = agent_mod.InvocationRequest
Message = agent_mod.Message

# ---------------------------------------------------------------------------
# Test harness
# ---------------------------------------------------------------------------

PASS = "✓"
FAIL = "✗"
_results: list[tuple[bool, str]] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    mark = PASS if condition else FAIL
    _results.append((condition, name))
    suffix = f" — {detail}" if detail else ""
    print(f"  {mark} {name}{suffix}")


# ---------------------------------------------------------------------------
# Fixtures — plain Python functions mimicking user-authored tools
# ---------------------------------------------------------------------------

from fastapi import params as _params
from pydantic import BaseModel


class _FakeWorkspace:
    pass


_WorkspaceDep = Annotated[_FakeWorkspace, _params.Depends(lambda: _FakeWorkspace())]


def get_weather(city: str, country_code: str = "US") -> str:
    """Get current weather for a city."""
    return f"72°F in {city}, {country_code}"


def query_genie(question: str, space_id: str, ws: _WorkspaceDep) -> str:  # type: ignore[valid-type]
    """Answer a question using a Genie Space."""
    return "some answer"


def no_args(ws: _WorkspaceDep) -> list[str]:  # type: ignore[valid-type]
    """List things."""
    return []


class StructuredOutput(BaseModel):
    answer: str
    confidence: float = 1.0


def structured_tool(x: int) -> StructuredOutput:
    """Returns structured output."""
    return StructuredOutput(answer=str(x))


# ---------------------------------------------------------------------------
# 1. _inspect_tool_fn
# ---------------------------------------------------------------------------

print("\n─── 1. _inspect_tool_fn ────────────────────────────────────────────")

plain, deps = _inspect_tool_fn(get_weather)
check("get_weather: city in plain_params", "city" in plain)
check("get_weather: country_code in plain_params", "country_code" in plain)
check("get_weather: no deps", deps == [])
check("get_weather: country_code default is 'US'", plain["country_code"][1] == "US")

plain, deps = _inspect_tool_fn(query_genie)
check("query_genie: question in plain", "question" in plain)
check("query_genie: space_id in plain", "space_id" in plain)
check("query_genie: ws excluded from plain", "ws" not in plain)
check("query_genie: ws in deps", "ws" in deps)

plain, deps = _inspect_tool_fn(no_args)
check("no_args: plain_params is empty", plain == {})
check("no_args: ws in deps", "ws" in deps)

# ---------------------------------------------------------------------------
# 2. _make_input_model
# ---------------------------------------------------------------------------

print("\n─── 2. _make_input_model ───────────────────────────────────────────")

plain_gw, _ = _inspect_tool_fn(get_weather)
model_gw = _make_input_model(get_weather, plain_gw)
check("get_weather: model is not None", model_gw is not None)
check("get_weather: 'city' field present", "city" in model_gw.model_fields)
check("get_weather: 'country_code' field present", "country_code" in model_gw.model_fields)

plain_qg, _ = _inspect_tool_fn(query_genie)
model_qg = _make_input_model(query_genie, plain_qg)
check("query_genie: 'question' and 'space_id' present",
      "question" in model_qg.model_fields and "space_id" in model_qg.model_fields)
check("query_genie: 'ws' NOT in model", "ws" not in model_qg.model_fields)

plain_na, _ = _inspect_tool_fn(no_args)
model_na = _make_input_model(no_args, plain_na)
check("no_args: model is None (no plain params)", model_na is None)

# ---------------------------------------------------------------------------
# 3. Agent.build_local_tools + _build_fmapi_tool_schemas
# ---------------------------------------------------------------------------

print("\n─── 3. Agent.build_local_tools + _build_fmapi_tool_schemas ─────────")

agent = Agent(tools=[get_weather, query_genie])
local_tools = agent.build_local_tools()

check("agent has 2 local tools", len(local_tools) == 2)

wt = next(t for t in local_tools if t.name == "get_weather")
qt = next(t for t in local_tools if t.name == "query_genie")

check("get_weather: description from docstring", "weather" in wt.description.lower())
check("get_weather: input_schema is not None", wt.input_schema is not None)
check("get_weather: schema has 'city'", "city" in wt.input_schema.get("properties", {}))
check("get_weather: 'ws' absent from schema", "ws" not in wt.input_schema.get("properties", {}))
check("get_weather: output_schema is string type", wt.output_schema == {"type": "string"})

check("query_genie: 'ws' absent from schema",
      "ws" not in qt.input_schema.get("properties", {}))
check("query_genie: 'question' and 'space_id' in schema",
      "question" in qt.input_schema.get("properties", {})
      and "space_id" in qt.input_schema.get("properties", {}))

schemas = _build_fmapi_tool_schemas(local_tools)
check("FMAPI: 2 tool schemas", len(schemas) == 2)
check("FMAPI: all type='function'", all(s["type"] == "function" for s in schemas))
wf = next(s for s in schemas if s["function"]["name"] == "get_weather")
check("FMAPI get_weather: has description", bool(wf["function"].get("description")))
check("FMAPI get_weather: parameters.properties has 'city'",
      "city" in wf["function"]["parameters"].get("properties", {}))

# ---------------------------------------------------------------------------
# 4. Agent.build_router — patched handler signatures
# ---------------------------------------------------------------------------

print("\n─── 4. Agent.build_router ──────────────────────────────────────────")

router = agent.build_router()
route_paths = [r.path for r in router.routes]
check("router has /tools/get_weather", "/tools/get_weather" in route_paths)
check("router has /tools/query_genie", "/tools/query_genie" in route_paths)

gw_route = next(r for r in router.routes if r.path == "/tools/get_weather")
gw_sig = inspect.signature(gw_route.endpoint)
check("get_weather handler: 'body' present", "body" in gw_sig.parameters)
check("get_weather handler: no 'ws' (no DI params)", "ws" not in gw_sig.parameters)

qg_route = next(r for r in router.routes if r.path == "/tools/query_genie")
qg_sig = inspect.signature(qg_route.endpoint)
check("query_genie handler: 'body' present", "body" in qg_sig.parameters)
check("query_genie handler: 'ws' DI param present", "ws" in qg_sig.parameters)

# ---------------------------------------------------------------------------
# 5. Structured output schema
# ---------------------------------------------------------------------------

print("\n─── 5. Structured output ───────────────────────────────────────────")

agent3 = Agent(tools=[structured_tool])
tools3 = agent3.build_local_tools()
st = tools3[0]
check("structured_tool: outputSchema has 'properties'", "properties" in (st.output_schema or {}))
check("structured_tool: outputSchema has 'answer' property",
      "answer" in (st.output_schema or {}).get("properties", {}))

# ---------------------------------------------------------------------------
# 6. Protocol models
# ---------------------------------------------------------------------------

print("\n─── 6. Protocol models ─────────────────────────────────────────────")

req = InvocationRequest(input=[Message(role="user", content="Hello")])
check("InvocationRequest: parses correctly", req.input[0].content == "Hello")
check("InvocationRequest: stream defaults False", req.stream is False)
check("InvocationRequest: custom_inputs defaults empty", req.custom_inputs == {})

# ---------------------------------------------------------------------------
# 7. AgentCard skill generation
# ---------------------------------------------------------------------------

print("\n─── 7. AgentCard skill generation ──────────────────────────────────")

cfg = AgentConfig(name="test-agent", description="Test agent", model="dummy-model")
tools_for_card = agent.build_local_tools()
card = AgentCard(
    name=cfg.name,
    description=cfg.description,
    skills=[
        agent_mod.A2ASkill(
            id=t.name, name=t.name, description=t.description,
            inputSchema=t.input_schema, outputSchema=t.output_schema,
        )
        for t in tools_for_card
    ],
)
check("card: name matches config", card.name == "test-agent")
check("card: 2 skills", len(card.skills) == 2)
check("card: schemaVersion is '1.0'", card.schemaVersion == "1.0")
check("card: protocolVersion present", bool(card.protocolVersion))
check("card: authSchemes present", len(card.authSchemes) > 0)
check("card: 'ws' absent from get_weather skill inputSchema",
      "ws" not in (card.skills[0].inputSchema or {}).get("properties", {}))

card_json = json.loads(card.model_dump_json())
check("card: JSON has 'schemaVersion'", "schemaVersion" in card_json)
check("card: JSON has 'capabilities'", "capabilities" in card_json)
check("card: JSON has 'provider'", "provider" in card_json)
check("card: JSON has 'skills'", "skills" in card_json)

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

print(f"\n{'─' * 60}")
passed = sum(1 for ok, _ in _results if ok)
total = len(_results)
failed_names = [name for ok, name in _results if not ok]
print(f"{PASS} {passed}/{total} checks passed")
if failed_names:
    print("\nFailed:")
    for name in failed_names:
        print(f"  {FAIL} {name}")
    sys.exit(1)
else:
    print("All good.")
