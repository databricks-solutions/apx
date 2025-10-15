from .config import conf
from databricks.sdk import WorkspaceClient

class Runtime:
    def __init__(self):
        self.config = conf

    @property
    def ws(self) -> WorkspaceClient:
        # note - this workspace client is usually an SP-based client
        # in development it usually uses the DATABRICKS_PROFILE
        return WorkspaceClient()
    
rt = Runtime()