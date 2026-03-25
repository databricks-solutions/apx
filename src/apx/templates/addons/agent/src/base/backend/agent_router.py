"""Example agent routes — delete or replace with your own tools.

Each route with an `operation_id` becomes an agent tool when
[tool.apx.agent] is configured in pyproject.toml.

  Tool name        = operation_id
  Tool description = docstring
  Input schema     = request body Pydantic model
  Output schema    = response_model
"""

from pydantic import BaseModel

from .core import Dependencies, create_router

router = create_router()


# ---------------------------------------------------------------------------
# Models
# ---------------------------------------------------------------------------


class CatalogSummary(BaseModel):
    name: str
    comment: str | None = None


class ListCatalogsOut(BaseModel):
    catalogs: list[CatalogSummary]


# ---------------------------------------------------------------------------
# Routes (= agent tools)
# ---------------------------------------------------------------------------


@router.get("/catalogs", response_model=ListCatalogsOut, operation_id="listCatalogs")
def list_catalogs(user_ws: Dependencies.UserClient) -> ListCatalogsOut:
    """List Unity Catalog catalogs accessible to the current user."""
    catalogs = [
        CatalogSummary(name=c.name or "", comment=c.comment)
        for c in user_ws.catalogs.list()
        if c.name
    ]
    return ListCatalogsOut(catalogs=catalogs)
