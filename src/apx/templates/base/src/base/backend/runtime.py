from databricks.sdk import WorkspaceClient

from .config import AppConfig


class Runtime:
    def __init__(self, config: AppConfig) -> None:
        self.config = config

    @property
    def ws(self) -> WorkspaceClient:
        # note - this workspace client is usually an SP-based client
        # in development it usually uses the DATABRICKS_CONFIG_PROFILE
        return WorkspaceClient()
