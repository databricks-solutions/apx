from __future__ import annotations

from abc import ABC, abstractmethod
from contextlib import asynccontextmanager
from inspect import isabstract
from typing import AsyncGenerator, Generic, TypeVar

from fastapi import FastAPI, Request
from pydantic_settings import BaseSettings, SettingsConfigDict

T = TypeVar("T")


class AddonConfig(BaseSettings):
    """Base configuration for addon dependencies.

    Subclasses only need to set ``model_config`` with their ``env_prefix``;
    ``extra="ignore"`` is inherited automatically.

    Example::

        class Config(AddonConfig):
            model_config = SettingsConfigDict(env_prefix="DATABRICKS_SQL_")
            warehouse_id: str = Field(description="SQL Warehouse ID")
    """

    model_config = SettingsConfigDict(extra="ignore")


class Dependency(ABC, Generic[T]):
    """Base class for all app dependencies (base and addons).

    Subclasses must implement initialize(), shutdown(), and get_instance().
    The lifespan() method provides a default async context manager that
    calls initialize/shutdown automatically.

    Concrete subclasses are auto-registered via __init_subclass__.
    """

    _registry: list[type[Dependency]] = []

    REGISTRY_NAME: str

    def __init_subclass__(cls, **kwargs: object) -> None:
        super().__init_subclass__(**kwargs)
        if not isabstract(cls) and cls not in Dependency._registry:
            Dependency._registry.append(cls)

    @abstractmethod
    async def initialize(self, app: FastAPI) -> None: ...

    @abstractmethod
    async def shutdown(self, app: FastAPI) -> None: ...

    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        await self.initialize(app)
        yield
        await self.shutdown(app)

    @abstractmethod
    def get_instance(self, request: Request) -> T: ...
