---
name: apx
description: Quick reference for building full-stack Databricks Apps with apx (React + FastAPI). Use when working on apx projects, creating routes, adding components, or managing dev servers.
user-invocable: true
---

# apx Toolkit

apx is the toolkit for building full-stack Databricks Apps with React + FastAPI.

## Prerequisites

Before using apx, verify the CLI is installed:
```bash
apx --version
```
If not installed, see https://github.com/databricks-solutions/apx for installation instructions.

## When to Use This Skill

- Working on a project that uses apx (check for `pyproject.toml` with apx entrypoint or `databricks.yml`)
- Creating or modifying FastAPI routes, Pydantic models, or React pages
- Managing dev servers, checking errors, or viewing logs
- Adding shadcn/ui components or frontend dependencies
- Deploying or debugging Databricks Apps

## Project Structure

```
src/<app>/
├── ui/                    # React + Vite frontend
│   ├── components/        # UI components (shadcn/ui)
│   ├── routes/            # @tanstack/react-router pages
│   ├── lib/               # Utilities (api client, selector)
│   └── styles/            # CSS styles
└── backend/               # FastAPI backend
    ├── app.py             # Main FastAPI app
    ├── router.py          # API routes
    ├── models.py          # Pydantic models
    └── core.py            # Config, logging, Dependency class, bootstrap
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `apx dev start` | Start all dev servers (backend + frontend + OpenAPI watcher) |
| `apx dev stop` | Stop all dev servers |
| `apx dev status` | Check status of running servers |
| `apx dev check` | Check for TypeScript/Python errors |
| `apx dev logs` | View recent logs (default: last 10m) |
| `apx dev logs -f` | Follow/stream logs in real-time |
| `apx build` | Build for production |
| `apx bun <args>` | Run bun commands (install, add, etc.) |
| `apx components add <name>` | Add a shadcn/ui component |

## MCP Tools

When the apx MCP server is running, these tools are available:

| Tool | Description |
|------|-------------|
| `start` | Start development server and return the URL |
| `stop` | Stop the development server |
| `restart` | Restart development server (preserves port if possible) |
| `logs` | Fetch recent dev server logs |
| `check` | Check project code for errors (tsc + ty in parallel) |
| `routes` | List all API routes to understand the project's API surface |
| `get_route_info` | Get code example for using a specific API route |
| `refresh_openapi` | Regenerate OpenAPI schema and API client |
| `search_registry_components` | Search shadcn registry components (semantic search) |
| `add_component` | Add a component to the project |
| `list_registry_components` | List all available shadcn registry components |
| `docs` | Search Databricks SDK docs for code examples |
| `databricks_apps_logs` | Fetch logs from deployed app via Databricks CLI |

## Recommended Workflow

1. **routes** — List all API routes to understand the project's API surface
2. **get_route_info** — Get a complete code example for a specific route
3. **search_registry_components** / **add_component** — Find and install UI components
4. **refresh_openapi** — Regenerate the API client after backend route changes
5. **check** — Run type checks to verify correctness
6. **start** / **restart** — Start or restart the dev server to test changes
7. **logs** — Diagnose runtime errors if something goes wrong

## Do's and Don'ts

- OpenAPI client auto-regenerates on code changes when dev servers are running — don't manually regenerate.
- Prefer running apx related commands via MCP server if it's available.
- Use the apx MCP `search_registry_components` and `add_component` tools to find and add shadcn/ui components.
- When using the API calls on the frontend, use error boundaries to handle errors.
- Run `apx dev check` command (via CLI or MCP) to check for errors in the project code after making changes.
- If agent has access to native browser tool, use it to verify changes on the frontend. If such tool is not present or is not working, use playwright MCP to automate browser actions.
- **Databricks SDK:** Use the apx MCP `docs` tool to search Databricks SDK documentation instead of guessing or hallucinating API signatures.

### Package Management

- **Frontend:** Use `apx bun install` or `apx bun add <dependency>` for frontend package management.
- **Python:** Always use `uv` (never `pip`).

### Component Management

- **Finding components:** Use MCP `search_registry_components` to search for available shadcn/ui components.
- **Adding components:** Use MCP `add_component` or CLI `apx components add <component> --yes` to add components.
- **Component location:** If component was added to a wrong location (e.g. stored into `src/components` instead of `src/<app>/ui/components`), move it to the proper folder.
- **Component organization:** Prefer grouping components by functionality rather than by file type (e.g. `src/<app>/ui/components/chat/`).

## Reference Files

For detailed patterns and code examples, see:
- [Backend Patterns](backend-patterns.md) — DI, 3-model pattern, CRUD routers, lifespan, AppConfig
- [Frontend Patterns](frontend-patterns.md) — Suspense, mutations, selector, component conventions

## Resources

- OpenAPI client: `src/<app>/ui/lib/api/` (auto-generated)
- Routes: `src/<app>/ui/routes/`
- Components: `src/<app>/ui/components/`
- Backend: `src/<app>/backend/`
