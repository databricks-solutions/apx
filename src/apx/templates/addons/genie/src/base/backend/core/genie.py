"""Genie Space client dependency."""
# /// apx
# exports = ["from .genie import GenieDependency, get_genie"]
# imports = [
#     "from .genie import get_genie",
#     "from databricks.sdk.service.dashboards import GenieAPI",
# ]
# aliases = ["GenieSpace: TypeAlias = Annotated[GenieAPI, Depends(get_genie)]"]
# ///

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.dashboards import GenieAPI
from fastapi import FastAPI, Request
from pydantic import Field
from pydantic_settings import SettingsConfigDict

from ._base import AddonConfig, Dependency


class GenieDependency(Dependency[GenieAPI]):
    REGISTRY_NAME = "GenieSpace"

    class Config(AddonConfig):
        model_config = SettingsConfigDict(env_prefix="DATABRICKS_GENIE_")
        space_id: str = Field(description="Genie Space ID")

    async def initialize(self, app: FastAPI) -> None:
        self.config = self.Config()  # ty: ignore[missing-argument]
        app.state.genie_config = self.config

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_instance(self, request: Request) -> GenieAPI:
        ws: WorkspaceClient = request.app.state.workspace_client
        return ws.genie


def get_genie(request: Request) -> GenieAPI:
    """Returns a Genie API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.genie
