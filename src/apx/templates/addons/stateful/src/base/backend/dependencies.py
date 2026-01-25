from functools import lru_cache
from typing import Annotated, Generator

from databricks.sdk import WorkspaceClient
from fastapi import Depends, Header
from sqlmodel import Session

from .config import AppConfig
from .runtime import Runtime


@lru_cache
def get_config() -> AppConfig:
    return AppConfig()


ConfigDep = Annotated[AppConfig, Depends(get_config)]


@lru_cache
def get_runtime(config: AppConfig = Depends(get_config)) -> Runtime:
    return Runtime(config)


RuntimeDep = Annotated[Runtime, Depends(get_runtime)]


def get_obo_ws(
    token: Annotated[str | None, Header(alias="X-Forwarded-Access-Token")] = None,
) -> WorkspaceClient:
    """
    Returns a Databricks Workspace client with authentication behalf of user.
    If the request contains an X-Forwarded-Access-Token header, on behalf of user authentication is used.

    Example usage:
    @api.get("/items/")
    async def read_items(obo_ws: Annotated[WorkspaceClient, Depends(get_obo_ws)]):
        # do something with the obo_ws
        ...
    """

    if not token:
        raise ValueError(
            "OBO token is not provided in the header X-Forwarded-Access-Token"
        )

    return WorkspaceClient(
        token=token, auth_type="pat"
    )  # set pat explicitly to avoid issues with SP client


def get_session(rt: RuntimeDep) -> Generator[Session, None, None]:
    """
    Returns a SQLModel session.
    """
    with rt.get_session() as session:
        yield session


SessionDep = Annotated[Session, Depends(get_session)]
