from databricks.sdk import WorkspaceClient
from fastapi import Header
from typing import Annotated
from .config import conf


def get_user_ws(
    token: Annotated[str | None, Header("X-Forwarded-Access-Token")] = None,
) -> WorkspaceClient:
    """
    Returns a Databricks Workspace client with authentication behalf of user.
    If the request contains an X-Forwarded-Access-Token header, on behalf of user authentication is used.

    Example usage:
    @api.get("/items/")
    async def read_items(obo_ws: Annotated[WorkspaceClient, Depends(get_user_ws)]):
        # do something with the obo_ws
        ...
    """
    if not token:
        raise ValueError(
            "No token for authentication provided in request headers or environment variables"
        )

    return WorkspaceClient(
        token=token, auth_type="pat"
    )  # set pat explicitly to avoid issues with SP client
