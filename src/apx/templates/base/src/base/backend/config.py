from importlib import resources
from pydantic_settings import BaseSettings, SettingsConfigDict
from pathlib import Path
from pydantic import Field
from dotenv import load_dotenv
from .utils import read_metadata, Metadata

# project root is the parent of the src folder
project_root = Path(__file__).parent.parent.parent
env_file = project_root / ".env"

if env_file.exists():
    load_dotenv(dotenv_path=env_file)

metadata = read_metadata()


class AppConfig(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=env_file, env_prefix=f"{metadata.app_name.upper()}_", extra="allow"
    )
    app_name: str = Field(default=metadata.app_name)
    api_prefix: str = Field(default="/api")
    metadata: Metadata = Field(default=metadata)

    @property
    def static_assets_path(self) -> Path:
        return resources.files(self.app_name).joinpath("__dist__")  # type: ignore


conf = AppConfig()
