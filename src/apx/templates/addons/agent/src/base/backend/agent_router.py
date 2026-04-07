"""Agent tools — replace with your own.

Each function becomes a tool: type hints define the schema, the docstring
is the description. Dependencies.* parameters are injected by FastAPI and
excluded from the tool schema.

The three tools below form a working Unity Catalog browser — ask the agent
"what catalogs do I have?", "list schemas in <catalog>", or
"show me the tables in <catalog>.<schema>". Use them as a pattern to build
your own tools, then remove or replace them.
"""

from .core import Dependencies
from .core.agent import Agent

AppClient = Dependencies.Client      # app service principal — works locally + in prod
UserClient = Dependencies.UserClient # logged-in user's identity (OBO) — prod only


# ---------------------------------------------------------------------------
# Example tools: Unity Catalog browser
# Replace these with your own tools when you're ready.
# ---------------------------------------------------------------------------

def list_catalogs(ws: AppClient) -> list[str]:
    """List Unity Catalog catalogs the app can access."""
    return [c.name for c in ws.catalogs.list() if c.name]


def list_schemas(catalog: str, ws: AppClient) -> list[str]:
    """List schemas inside a Unity Catalog catalog.
    catalog: catalog name (e.g. 'main' or 'my_catalog')"""
    return [s.name for s in ws.schemas.list(catalog_name=catalog) if s.name]


def list_tables(catalog: str, schema: str, ws: AppClient) -> list[str]:
    """List tables inside a Unity Catalog schema.
    catalog: catalog name (e.g. 'main')
    schema: schema name (e.g. 'default')"""
    return [
        t.name for t in ws.tables.list(catalog_name=catalog, schema_name=schema) if t.name
    ]


agent = Agent(tools=[list_catalogs, list_schemas, list_tables])


# ---------------------------------------------------------------------------
# What else you can do (visit /_apx/setup for guided configuration)
# ---------------------------------------------------------------------------
#
# LoopAgent — let the agent iterate until the task is done:
#   Wrap your Agent: agent = LoopAgent(Agent(tools=[...]), max_iterations=10)
#   Or toggle from /_apx/setup (Agent Pattern section).
#
# RAG — Mosaic AI Vector Search:
#   Set vector_search_index in pyproject.toml [tool.apx.agent], or visit
#   /_apx/setup to discover indexes and generate a search tool automatically.
#
# Sub-agents: consume other agents as tools (A2A)
# ---------------------------------------------------------------------------
# Your agent can call other APX agents as tools. Each sub-agent's skills are
# auto-discovered via /.well-known/agent.json and exposed to the LLM.
# OBO auth tokens are forwarded automatically at every hop.
#
# Option A — config-driven (recommended for deploy flexibility):
#   In pyproject.toml [tool.apx.agent]:
#     sub_agents = ["$PRICING_AGENT_URL", "$INVENTORY_AGENT_URL"]
#   Then set env vars per environment:
#     PRICING_AGENT_URL=http://localhost:8001          # dev
#     PRICING_AGENT_URL=https://pricing.ws.databricksapps.com  # prod
#
# Option B — hardcoded in code:
#   agent = Agent(
#       tools=[list_catalogs, list_schemas, list_tables],
#       sub_agents=["https://pricing.ws.databricksapps.com"],
#   )
#
# ---------------------------------------------------------------------------
# MAS (Multi-Agent Supervisor) compatibility
# ---------------------------------------------------------------------------
# This agent is MAS-consumable out of the box. It exposes:
#   GET  /.well-known/agent.json  — A2A discovery card (name, skills, auth)
#   POST /invocations              — MLflow ResponsesAgent protocol
#   POST /mcp                      — MCP stateless HTTP transport
# To register in MAS: deploy this app, then add its URL as a sub-agent.
#
# Auth chain: User → MAS (OBO) → This Agent (OBO forwarded) → Sub-Agents
# Authorization + X-Forwarded-Access-Token headers pass through every hop.
