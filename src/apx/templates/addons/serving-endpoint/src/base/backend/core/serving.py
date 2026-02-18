"""Serving Endpoint client dependency."""
# /// apx
# exports = ["from .serving import ServingDependency, get_serving_endpoint"]
# imports = [
#     "from .serving import get_serving_endpoint",
#     "from databricks.sdk.service.serving import ServingEndpointsAPI",
# ]
# aliases = ["ServingEndpoint: TypeAlias = Annotated[ServingEndpointsAPI, Depends(get_serving_endpoint)]"]
# ///

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.serving import ServingEndpointsAPI
from fastapi import FastAPI, Request

from ._base import Dependency


class ServingDependency(Dependency[ServingEndpointsAPI]):
    REGISTRY_NAME = "ServingEndpoint"

    async def initialize(self, app: FastAPI) -> None:
        pass

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_instance(self, request: Request) -> ServingEndpointsAPI:
        ws: WorkspaceClient = request.app.state.workspace_client
        return ws.serving_endpoints


def get_serving_endpoint(request: Request) -> ServingEndpointsAPI:
    """Returns a Serving Endpoints API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.serving_endpoints
