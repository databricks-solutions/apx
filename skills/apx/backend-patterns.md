# Backend Patterns

## Technology Stack

- **Framework:** FastAPI + Pydantic
- **Package manager:** uv (never pip)
- **Database ORM:** SQLModel (stateful apps only)
- **Auth:** Databricks WorkspaceClient (service principal or OBO)

## 3-Model Pattern

Every API entity uses three Pydantic models:

| Model | Purpose | Example |
|-------|---------|---------|
| `Entity` | Internal/database model | `Item` |
| `EntityIn` | Input/request body | `ItemIn` |
| `EntityOut` | Output/response model | `ItemOut` |

```python
from pydantic import BaseModel

# Internal/DB model
class Item(BaseModel):
    id: str
    name: str
    description: str | None = None
    created_at: datetime

# Input model — only fields the client sends
class ItemIn(BaseModel):
    name: str
    description: str | None = None

# Output model — what the API returns
class ItemOut(BaseModel):
    id: str
    name: str
    description: str | None = None
    created_at: datetime
```

## CRUD Router Template

API routes **must** include `response_model` and `operation_id` for correct client generation.

The `operation_id` maps directly to the generated TypeScript hook name:
- `operation_id="listItems"` → `useListItems()` / `useListItemsSuspense()`
- `operation_id="createItem"` → `useCreateItem()`
- `operation_id="getItem"` → `useGetItem()` / `useGetItemSuspense()`
- `operation_id="updateItem"` → `useUpdateItem()`
- `operation_id="deleteItem"` → `useDeleteItem()`

```python
from fastapi import APIRouter

router = APIRouter(prefix="/api")

@router.get("/items", response_model=list[ItemOut], operation_id="listItems")
async def list_items(page: int = 1, page_size: int = 20):
    ...

@router.post("/items", response_model=ItemOut, operation_id="createItem")
async def create_item(item: ItemIn):
    ...

@router.get("/items/{item_id}", response_model=ItemOut, operation_id="getItem")
async def get_item(item_id: str):
    ...

@router.put("/items/{item_id}", response_model=ItemOut, operation_id="updateItem")
async def update_item(item_id: str, item: ItemIn):
    ...

@router.delete("/items/{item_id}", operation_id="deleteItem")
async def delete_item(item_id: str):
    ...
```

## Dependencies and Dependency Injection

The `Dependency` class in `src/<app>/backend/core.py` provides typed FastAPI dependencies. **Always use these instead of manually creating clients or accessing `request.app.state`.**

| Dependency | Type | Description |
|---|---|---|
| `Dependency.Client` | `WorkspaceClient` | Databricks client using app-level service principal credentials |
| `Dependency.UserClient` | `WorkspaceClient` | Databricks client authenticated on behalf of the current user (requires OBO token) |
| `Dependency.Config` | `AppConfig` | Application configuration loaded from environment variables |
| `Dependency.Session` | `Session` | SQLModel database session, scoped to request (stateful apps only) |

### Usage in Route Handlers

```python
from .core import Dependency, create_router

router = create_router()

# Service principal client
@router.get("/clusters", response_model=list[ClusterOut], operation_id="listClusters")
def list_clusters(ws: Dependency.Client):
    return ws.clusters.list()

# User-scoped client (OBO)
@router.get("/me", response_model=UserOut, operation_id="currentUser")
def me(user_ws: Dependency.UserClient):
    return user_ws.current_user.me()

# Application config
@router.get("/settings", response_model=AppSettingsOut, operation_id="getSettings")
def get_settings(config: Dependency.Config):
    return AppSettingsOut(app_name=config.app_name)

# Database session (stateful apps only)
@router.get("/orders", response_model=list[OrderOut], operation_id="getOrders")
def get_orders(session: Dependency.Session):
    return session.exec(select(Order)).all()
```

## Extending AppConfig

Add custom fields to `AppConfig` in `core.py`. Fields are populated from environment variables with `{APP_SLUG}_` prefix:

```python
class AppConfig(BaseSettings):
    app_name: str = Field(default=app_name)
    my_setting: str = Field(default="value")  # env var: {APP_SLUG}_MY_SETTING
```

## Custom Lifespan

Use the `lifespan` parameter in `create_app` for startup/shutdown logic. The default lifespan (config + workspace client) runs first:

```python
from contextlib import asynccontextmanager
from fastapi import FastAPI

@asynccontextmanager
async def custom_lifespan(app: FastAPI):
    # app.state.config and app.state.workspace_client already available
    app.state.my_resource = await init_something(app.state.config)
    yield

app = create_app(routers=[router], lifespan=custom_lifespan)
```

## Project Layout

```
src/<app>/backend/
├── app.py             # FastAPI entrypoint: app = create_app(routers=[router], lifespan=...)
├── router.py          # API routes with operation_id
├── models.py          # Pydantic models (Entity, EntityIn, EntityOut)
└── core.py            # Dependency class, AppConfig, create_router, create_app
```

Backend serves the frontend at `/` and the API at `/api`.
