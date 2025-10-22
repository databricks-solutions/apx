from importlib import resources
from pydantic_settings import BaseSettings, SettingsConfigDict
from pathlib import Path
from pydantic import Field
from dotenv import load_dotenv
from .._metadata import app_name
from .logger import logger

# project root is the parent of the src folder
project_root = Path(__file__).parent.parent.parent.parent
env_file = project_root / ".env"

if env_file.exists():
    logger.info(f"Loading environment variables from {env_file.resolve()}")
    load_dotenv(dotenv_path=env_file)


class AppConfig(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=env_file, env_prefix=f"{app_name.upper()}_", extra="allow"
    )
    app_name: str = Field(default=app_name)
    api_prefix: str = Field(default="/api")

    @property
    def static_assets_path(self) -> Path:
        return resources.files(self.app_name).joinpath("__dist__")  # type: ignore


conf = AppConfig()
logger.info(f"Application configuration: {conf.model_dump_json(indent=2)}")
