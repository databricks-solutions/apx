from __future__ import annotations

from pydantic_settings import BaseSettings, SettingsConfigDict

from ..._metadata import app_slug


class DatabricksResourcesConfig(BaseSettings):
    """Configuration for Databricks resources injected via environment variables.

    Environment variables are prefixed with ``{APP_SLUG}_RESOURCE_``.
    """

    model_config = SettingsConfigDict(
        env_prefix=f"{app_slug.upper()}_RESOURCE_", extra="ignore"
    )
    warehouse_id: str | None = None
    lakebase_instance_name: str | None = None
    serving_endpoint_name: str | None = None
