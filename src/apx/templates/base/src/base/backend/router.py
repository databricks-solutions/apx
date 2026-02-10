from databricks.sdk.service.iam import User as UserOut
from fastapi import APIRouter

from .._metadata import api_prefix
from .core import Dependency
from .models import VersionOut

api = APIRouter(prefix=api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()


@api.get("/current-user", response_model=UserOut, operation_id="currentUser")
def me(user_ws: Dependency.UserClient):
    return user_ws.current_user.me()
