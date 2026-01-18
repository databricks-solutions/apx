# interop utilities to provide interface into pythonic data structures from rust
# primary reason: Databricks SDK doesn't have rust bindings, therefore we need to use pythonic data structures

from apx._core import get_dotenv_vars
from apx import __version__
import os


def apply_dotenv_vars() -> None:
    dotenv_vars = get_dotenv_vars()
    for env_var, value in dotenv_vars.items():
        os.environ[env_var] = value


def get_token() -> str:
    apply_dotenv_vars()

    from databricks.sdk import WorkspaceClient

    ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    token = ws.config.oauth_token().access_token
    return token


def credentials_valid() -> tuple[bool, str]:
    """Returns (is_valid, error_message). error_message is empty if valid."""
    apply_dotenv_vars()
    from databricks.sdk import WorkspaceClient

    ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    try:
        ws.current_user.me()
        return (True, "")
    except Exception as e:
        return (False, str(e))


def get_forwarded_user_header() -> str:
    apply_dotenv_vars()

    from databricks.sdk import WorkspaceClient

    ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    user_id = ws.current_user.me().id
    assert user_id is not None, "User ID is not set"
    try:
        workspace_id = ws.config.host.split("-")[1].split(".")[0]
    except Exception:
        workspace_id = "placeholder"
    return f"{user_id}@{workspace_id}"
