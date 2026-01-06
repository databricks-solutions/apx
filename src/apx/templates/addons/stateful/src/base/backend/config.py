from importlib import resources
from typing import ClassVar
from pydantic_settings import BaseSettings, SettingsConfigDict
from pathlib import Path
from pydantic import Field
from dotenv import load_dotenv
from .._metadata import app_name, app_slug, api_prefix

# project root is the parent of the src folder
project_root = Path(__file__).parent.parent.parent.parent
env_file = project_root / ".env"

if env_file.exists():
    load_dotenv(dotenv_path=env_file)


class DatabaseConfig(BaseSettings):
    model_config: ClassVar[SettingsConfigDict] = SettingsConfigDict(
        extra="ignore",
    )
    port: int = Field(description="The port of the database", default=5432)
    database_name: str = Field(
        description="The name of the database", default="databricks_postgres"
    )
    instance_name: str = Field(
        description="The name of the database instance",
        validation_alias="PGAPPNAME",  # Reads directly from PGAPPNAME env var
    )


class AppConfig(BaseSettings):
    model_config: ClassVar[SettingsConfigDict] = SettingsConfigDict(
        env_file=env_file,
        env_prefix=f"{app_slug.upper()}_",
        extra="ignore",
        env_nested_delimiter="__",
    )
    app_name: str = Field(default=app_name)
    api_prefix: str = Field(default=api_prefix)
    db: DatabaseConfig = DatabaseConfig()  # pyright: ignore[reportCallIssue]

    @property
    def static_assets_path(self) -> Path:
        return Path(str(resources.files(app_slug))).joinpath("__dist__")


conf = AppConfig()
