from databricks.sdk.service.iam import User as UserOut

from .core import Dependency, create_router
from .models import VersionOut

router = create_router()


@router.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()


@router.get("/current-user", response_model=UserOut, operation_id="currentUser")
def me(user_ws: Dependency.UserClient):
    return user_ws.current_user.me()
