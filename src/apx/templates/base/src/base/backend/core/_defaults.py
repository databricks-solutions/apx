from __future__ import annotations

from databricks.sdk import WorkspaceClient
from fastapi import FastAPI, Request

from ._base import Dependency
from ._config import AppConfig, logger


class ConfigDependency(Dependency[AppConfig]):
    REGISTRY_NAME = "Config"

    async def initialize(self, app: FastAPI) -> None:
        config = AppConfig()
        logger.info(f"Starting app with configuration:\n{config}")
        app.state.config = config

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_instance(self, request: Request) -> AppConfig:
        return request.app.state.config


class WorkspaceClientDependency(Dependency[WorkspaceClient]):
    REGISTRY_NAME = "Client"

    async def initialize(self, app: FastAPI) -> None:
        app.state.workspace_client = WorkspaceClient()

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_instance(self, request: Request) -> WorkspaceClient:
        return request.app.state.workspace_client
