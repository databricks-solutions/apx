from typing import Annotated
from fastapi import APIRouter, Depends
from .models import VersionOut
from databricks.sdk import WorkspaceClient
from databricks.sdk.service.iam import User as UserOut
from .dependencies import get_obo_ws
from .config import conf

api = APIRouter(prefix=conf.api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()


@api.get("/me", response_model=UserOut, operation_id="me")
def me(obo_ws: Annotated[WorkspaceClient, Depends(get_obo_ws)]):
    return obo_ws.current_user.me()
