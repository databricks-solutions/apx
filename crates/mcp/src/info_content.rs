/// Static informational content describing the apx toolkit.
///
/// Covers project structure, key patterns, and available tools.
/// Served as the `apx://info` resource and included in the server's `instructions` field.
pub const APX_INFO_CONTENT: &str = r#"
This project uses apx toolkit to build a Databricks app.
apx bundles together a set of tools and libraries to help you with the complete app development lifecycle: develop, build and deploy.

## Technology Stack

- **Backend**: Python + FastAPI + Pydantic
- **Frontend**: React + TypeScript + shadcn/ui
- **Build Tools**: uv (Python), bun (JavaScript/TypeScript)

## Project Structure

```
<project_root>/
├── pyproject.toml          # Python project config (app name, slug, entrypoint, api_prefix)
├── src/<app_slug>/          # Backend Python package
│   ├── app.py              # FastAPI entrypoint (app = FastAPI(...))
│   └── routers/            # FastAPI route modules
├── ui/                     # Frontend React app
│   ├── src/
│   │   ├── lib/api.ts      # Generated API client (Orval) — DO NOT edit manually
│   │   ├── lib/selector.ts # Default query selector
│   │   ├── components/ui/  # Installed shadcn components
│   │   └── routes/         # Page components (TanStack Router)
│   └── package.json
└── databricks.yml          # Databricks deployment config
```

## Key Frontend Patterns

### Query with Suspense (recommended)
```tsx
import { Suspense } from "react";
import { QueryErrorResetBoundary } from "@tanstack/react-query";
import { ErrorBoundary } from "react-error-boundary";
import { Skeleton } from "@/components/ui/skeleton";
import { useListItemsSuspense } from "@/lib/api";
import selector from "@/lib/selector";

function ItemsContent() {
  const { data } = useListItemsSuspense(selector());
  return <div>{/* render data */}</div>;
}

export function ItemsPage() {
  return (
    <QueryErrorResetBoundary>
      {({ reset }) => (
        <ErrorBoundary onReset={reset} fallbackRender={({ resetErrorBoundary }) => (
          <div>
            <p>Something went wrong</p>
            <button onClick={resetErrorBoundary}>Try again</button>
          </div>
        )}>
          <Suspense fallback={<Skeleton className="h-48 w-full" />}>
            <ItemsContent />
          </Suspense>
        </ErrorBoundary>
      )}
    </QueryErrorResetBoundary>
  );
}
```

### Mutation with cache invalidation
```tsx
import { useCreateItem } from "@/lib/api";
import { useQueryClient } from "@tanstack/react-query";

function CreateItemButton() {
  const queryClient = useQueryClient();
  const { mutate, isPending } = useCreateItem({
    mutation: {
      onSuccess: () => {
        queryClient.invalidateQueries({ queryKey: ["listItems"] });
      },
    },
  });

  return (
    <button onClick={() => mutate({ data: { name: "New item" } })} disabled={isPending}>
      {isPending ? "Creating..." : "Create"}
    </button>
  );
}
```

### selector() usage
- No params: `useListItemsSuspense(selector())`
- With params: `useListItemsSuspense({ params: { page, page_size }, ...selector() })`

## Key Backend Patterns

### FastAPI router with operation_id
```python
from fastapi import APIRouter

router = APIRouter(prefix="/api")

@router.get("/items", operation_id="listItems")
async def list_items(page: int = 1, page_size: int = 20):
    ...

@router.post("/items", operation_id="createItem")
async def create_item(item: ItemCreate):
    ...
```

The `operation_id` is used to generate TypeScript hooks (e.g., `useListItems`, `useCreateItem`).

### SDK-First Rule
Always use `databricks-sdk` (`WorkspaceClient`) for Databricks operations — never raw HTTP calls.
SDK listing methods (`ws.jobs.list()`, etc.) return lazy iterators that auto-paginate.
SDK dataclasses are Pydantic-compatible — use them directly in `response_model` or compose into custom models.
Use the `docs` tool to verify method signatures before writing SDK code.

### Streaming Endpoints
For SSE/streaming (e.g. chat, agent): use `StreamingResponse` with `text/event-stream` on the backend.
On the frontend, use manual `fetch()` + `ReadableStream` — not generated React Query hooks.
See backend-patterns.md and frontend-patterns.md for complete examples.

## Workflow

1. **routes** — List all API routes to understand the project's API surface
2. **get_route_info** — Get a complete code example for a specific route
3. **search_registry_components** / **add_component** — Find and install UI components
4. **refresh_openapi** — Regenerate the API client after backend route changes
5. **check** — Run type checks to verify correctness
6. **start** / **restart** — Start or restart the dev server to test changes
7. **logs** — Diagnose runtime errors if something goes wrong

## Tool Usage

All project-scoped tools require an `app_path` parameter — the absolute path to the project directory.
Global tools (like `docs`) do not require `app_path`.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_content_contains_key_sections() {
        assert!(APX_INFO_CONTENT.contains("Project Structure"));
        assert!(APX_INFO_CONTENT.contains("Frontend Patterns"));
        assert!(APX_INFO_CONTENT.contains("Backend Patterns"));
        assert!(APX_INFO_CONTENT.contains("SDK-First Rule"));
        assert!(APX_INFO_CONTENT.contains("Streaming Endpoints"));
        assert!(APX_INFO_CONTENT.contains("Workflow"));
    }
}
