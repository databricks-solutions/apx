from __future__ import annotations

from abc import ABC, abstractmethod
from contextlib import asynccontextmanager
from inspect import isabstract
from typing import Any, AsyncGenerator, Generic, TypeVar

from fastapi import APIRouter, FastAPI, Request, Depends

T = TypeVar("T")


class LifespanDependency(ABC, Generic[T]):
    """
    All lifespan dependencies must inherit from this class.
    """

    _registry: list[type[LifespanDependency]] = []

    def __init_subclass__(cls, **kwargs: object) -> None:
        super().__init_subclass__(**kwargs)
        if not isabstract(cls) and cls not in LifespanDependency._registry:
            LifespanDependency._registry.append(cls)

    @abstractmethod
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        yield

    def get_routers(self) -> list[APIRouter]:
        """Override to contribute routers to the application."""
        return []

    @abstractmethod
    @classmethod
    def get_instance(cls, request: Request) -> T: ...

    @classmethod
    def as_depends(cls) -> Any:
        return Depends(cls.get_instance)
