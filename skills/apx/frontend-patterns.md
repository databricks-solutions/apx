# Frontend Patterns

## Technology Stack

- **Framework:** React + TypeScript
- **Build tool:** Vite + bun
- **Routing:** @tanstack/react-router
- **Data fetching:** @tanstack/react-query (via Orval-generated hooks)
- **Components:** shadcn/ui
- **API client:** Auto-generated from OpenAPI schema (Orval)

## Query with Suspense (Recommended Pattern)

Always use `useXSuspense` hooks with `Suspense` and `Skeleton` components for data fetching:

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

**Key rule:** Render static elements (headers, layout) immediately. Wrap only the data-fetching parts in `Suspense`.

## Mutation with Cache Invalidation

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

The query key for invalidation matches the `operation_id` from the backend route (e.g., `operation_id="listItems"` → query key `["listItems"]`).

## selector() Usage

The `selector()` function provides a default query selector for clean data destructuring:

```tsx
// No params — simple list query
const { data } = useListItemsSuspense(selector());

// With params — pass query parameters alongside selector
const { data } = useListItemsSuspense({
  params: { page, page_size },
  ...selector(),
});
```

## Component Conventions

- Use **shadcn/ui** components — add via MCP `add_component` or `apx components add <name> --yes`
- Store components in `src/<app>/ui/components/`
- Group by functionality: `src/<app>/ui/components/chat/`, `src/<app>/ui/components/dashboard/`
- If a component is installed to the wrong location (e.g., `src/components/`), move it to `src/<app>/ui/components/`

## Routing

Routes live in `src/<app>/ui/routes/` and use `@tanstack/react-router` file-based routing.

## Data Fetching Rules

- **Always** use `useXSuspense` hooks (not `useX` hooks) for page-level data loading
- **Always** wrap suspense queries in `Suspense` + `ErrorBoundary`
- **Always** provide a `Skeleton` fallback
- **Never** manually call `fetch()` or `axios` — use the generated API hooks

## Project Layout

```
src/<app>/ui/
├── components/        # UI components (shadcn/ui)
│   └── ui/            # Installed shadcn base components
├── routes/            # @tanstack/react-router pages
├── lib/
│   ├── api.ts         # Generated API client (Orval) — DO NOT edit manually
│   └── selector.ts    # Default query selector
└── styles/            # CSS styles
```
