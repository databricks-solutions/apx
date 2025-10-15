from fastapi import APIRouter
from .models import VersionOut
from .config import conf

api = APIRouter(prefix=conf.api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()
