from __future__ import annotations

from typing import Annotated
from uuid import UUID

from fastapi import Header
from pydantic import BaseModel, SecretStr


class DatabricksAppsHeaders(BaseModel):
    """Structured model for Databricks Apps HTTP headers.

    See: https://docs.databricks.com/aws/en/dev-tools/databricks-apps/http-headers
    """

    host: str
    user_name: str
    user_id: str
    user_email: str
    request_id: UUID
    token: SecretStr | None


def get_databricks_headers(
    host: Annotated[str | None, Header(alias="X-Forwarded-Host")] = None,
    user_name: Annotated[
        str | None, Header(alias="X-Forwarded-Preferred-Username")
    ] = None,
    user_id: Annotated[str | None, Header(alias="X-Forwarded-User")] = None,
    user_email: Annotated[str | None, Header(alias="X-Forwarded-Email")] = None,
    request_id: Annotated[str | None, Header(alias="X-Request-Id")] = None,
    token: Annotated[
        str | None, Header(alias="X-Forwarded-Access-Token")
    ] = None,
) -> DatabricksAppsHeaders:
    """Extract Databricks Apps headers from the incoming request."""
    return DatabricksAppsHeaders(
        host=host or "",
        user_name=user_name or "",
        user_id=user_id or "",
        user_email=user_email or "",
        request_id=request_id or "00000000-0000-0000-0000-000000000000",
        token=SecretStr(token) if token else None,
    )
