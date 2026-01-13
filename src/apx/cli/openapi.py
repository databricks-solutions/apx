"""OpenAPI schema generation and API client generation utilities."""

from __future__ import annotations

import json
import logging
import subprocess
from pathlib import Path
from typing import TYPE_CHECKING, Annotated, ClassVar

from pydantic import BaseModel, ConfigDict
from typer import Argument, Exit, Option

if TYPE_CHECKING:
    from fastapi import FastAPI

from apx.cli.version import with_version
from apx.models import ProjectMetadata
from apx.utils import (
    console,
    ensure_dir,
    in_path,
    progress_spinner,
)


class ApiGeneratorConfig(BaseModel):
    """Configuration for the API client generator."""

    app_dir: Path
    app_slug: str
    app_module_name: str

    model_config: ClassVar[ConfigDict] = ConfigDict(arbitrary_types_allowed=True)

    @classmethod
    def from_app_dir(cls, app_dir: Path) -> ApiGeneratorConfig:
        """Create config from app directory by reading project metadata."""
        with in_path(app_dir):
            metadata = ProjectMetadata.read()
            return cls(
                app_dir=app_dir,
                app_slug=metadata.app_slug,
                app_module_name=metadata.app_module,
            )


class ApiGenerator:
    """Generates OpenAPI schema and API client from a FastAPI app.

    This class encapsulates all the logic for:
    - Ensuring the client generator config exists
    - Generating the OpenAPI schema from the FastAPI app
    - Running the client generator to create TypeScript API client
    - Watching for changes and regenerating automatically
    """

    CONFIG_FILENAME: str = "orval.config.ts"
    SCHEMA_FILENAME: str = "openapi.json"
    APX_DIR_NAME: str = ".apx"

    def __init__(
        self, config: ApiGeneratorConfig, logger: logging.Logger | None = None
    ):
        """Initialize the API generator.

        Args:
            config: Configuration for the generator
            logger: Optional logger for output. If None, uses console.print()
        """
        self._config: ApiGeneratorConfig = config
        self._logger: logging.Logger | None = logger

    @property
    def app_dir(self) -> Path:
        """Get the app directory."""
        return self._config.app_dir

    @property
    def app_slug(self) -> str:
        """Get the app slug."""
        return self._config.app_slug

    @property
    def app_module_name(self) -> str:
        """Get the app module name."""
        return self._config.app_module_name

    @property
    def apx_dir(self) -> Path:
        """Get the .apx directory path."""
        return self.app_dir / self.APX_DIR_NAME

    @property
    def config_path(self) -> Path:
        """Get the client generator config file path."""
        return self.apx_dir / self.CONFIG_FILENAME

    @property
    def schema_path(self) -> Path:
        """Get the OpenAPI schema file path."""
        return self.apx_dir / self.SCHEMA_FILENAME

    def _log(self, message: str) -> None:
        """Log a message using logger or console."""
        if self._logger:
            self._logger.info(message)
        else:
            console.print(message)

    def _log_error(self, message: str) -> None:
        """Log an error message using logger or console."""
        if self._logger:
            self._logger.error(message)
        else:
            console.print(f"[red]{message}[/red]")

    def ensure_config(self) -> Path:
        """Ensure the client generator config file exists.

        Creates a default config file if it doesn't exist.

        Returns:
            Path to the config file
        """
        ensure_dir(self.apx_dir)

        if not self.config_path.exists():
            config_content = f"""import {{ defineConfig }} from "orval";

export default defineConfig({{
  api: {{
    input: ".apx/openapi.json",
    output: {{
      target: "../src/{self.app_slug}/ui/lib/api.ts",
      client: "react-query",
      httpClient: "axios",
      prettier: true,
      override: {{
        query: {{
          useQuery: true,
          useSuspenseQuery: true,
        }},
      }},
    }},
  }},
}});
"""
            self.config_path.write_text(config_content)
            console.print(
                f"[green]✓[/green] Created API client config at {self.config_path.relative_to(self.app_dir)}"
            )

        return self.config_path

    def generate_schema(self) -> tuple[Path, bool]:
        """Generate the OpenAPI schema JSON file.

        Always loads the app module fresh to ensure the latest schema.

        Returns:
            Tuple of (schema_path, schema_changed) where schema_changed indicates
            if the schema differs from the previous version
        """
        # Load the app instance fresh
        metadata = ProjectMetadata.read(self.app_dir)
        app_instance = metadata.get_app_instance(reload=True)

        # Generate OpenAPI spec
        spec = app_instance.openapi()
        new_spec_json = json.dumps(spec, indent=2)

        # Ensure .apx directory exists
        ensure_dir(self.apx_dir)

        # Check if schema has changed
        schema_changed = True
        if self.schema_path.exists():
            existing_spec = self.schema_path.read_text()
            if existing_spec == new_spec_json:
                schema_changed = False

        # Write the new schema if it changed
        if schema_changed:
            self.schema_path.write_text(new_spec_json)

        return self.schema_path, schema_changed

    def generate_client(self) -> None:
        """Run the client generator to create the TypeScript API client.

        Raises:
            Exit: If the client generation fails
        """
        result = subprocess.run(
            [
                "bun",
                "x",
                "--bun",
                "orval",
                "-i",
                str(self.schema_path.relative_to(self.app_dir)),
                "-c",
                str(self.config_path.relative_to(self.app_dir)),
            ],
            cwd=self.app_dir,
            capture_output=True,
            text=True,
        )

        if result.returncode != 0:
            self._log_error("Failed to generate API client")
            if result.stderr:
                console.print(f"[red]{result.stderr}[/red]")
            if result.stdout:
                console.print(f"[red]{result.stdout}[/red]")
            raise Exit(code=1)

    def run(self, force: bool = False) -> None:
        """Generate OpenAPI schema and API client.

        Args:
            force: If True, always regenerate client even if schema hasn't changed
        """
        with progress_spinner(
            "📝 Generating OpenAPI schema...", "✅ OpenAPI schema generated"
        ):
            schema_path, schema_changed = self.generate_schema()

        # Ensure config exists
        self.ensure_config()

        if schema_changed or force:
            with progress_spinner(
                "🔧 Generating API client...", "✅ API client generated"
            ):
                self.generate_client()
            if force and not schema_changed:
                console.print(
                    "[bold green]✨ API client forcefully regenerated![/bold green]"
                )
            else:
                console.print(
                    "[bold green]✨ OpenAPI schema and client generated successfully![/bold green]"
                )
        else:
            console.print("[dim]⏭️  Schema unchanged, skipping client generation[/dim]")
            console.print("[bold green]✨ OpenAPI schema is up to date![/bold green]")


def create_api_generator(
    app_dir: Path, logger: logging.Logger | None = None
) -> ApiGenerator:
    """Factory function to create an ApiGenerator from an app directory.

    Args:
        app_dir: The path to the app directory
        logger: Optional logger for output

    Returns:
        Configured ApiGenerator instance

    Raises:
        Exit: If project metadata cannot be read
    """
    try:
        config = ApiGeneratorConfig.from_app_dir(app_dir)
    except Exception as e:
        console.print(f"[red]❌ Failed to read project metadata: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you're in a valid apx project directory[/yellow]"
        )
        raise Exit(code=1)

    return ApiGenerator(config, logger=logger)


def run_openapi(app_dir: Path, force: bool = False) -> None:
    """Generate OpenAPI schema and API client.

    Args:
        app_dir: The path to the app directory
        force: If True, always regenerate client even if schema hasn't changed
    """
    generator = create_api_generator(app_dir)
    generator.run(force=force)


def regenerate_openapi_if_changed(
    app: "FastAPI",
    app_dir: Path,
    logger: logging.Logger | None = None,
) -> bool:
    """Regenerate OpenAPI schema and client if the app's schema has changed.

    This is a simple function meant to be called after the app is reloaded.
    It compares the app's current OpenAPI schema with the existing .apx/openapi.json
    and regenerates if different.

    Args:
        app: The FastAPI application instance (already loaded)
        app_dir: The application directory
        logger: Optional logger for output

    Returns:
        True if schema was regenerated, False if unchanged
    """
    from fastapi import FastAPI

    if not isinstance(app, FastAPI):
        raise TypeError(f"Expected FastAPI instance, got {type(app)}")

    apx_dir = app_dir / ".apx"
    schema_path = apx_dir / "openapi.json"

    # Generate current schema from app
    spec = app.openapi()
    new_spec_json = json.dumps(spec, indent=2)

    # Check if schema has changed
    schema_changed = True
    if schema_path.exists():
        existing_spec = schema_path.read_text()
        if existing_spec == new_spec_json:
            schema_changed = False

    if not schema_changed:
        if logger:
            logger.info("OpenAPI schema unchanged, skipping regeneration")
        return False

    # Write new schema
    ensure_dir(apx_dir)
    schema_path.write_text(new_spec_json)

    if logger:
        logger.info("OpenAPI schema changed, regenerating client...")

    # Generate client using orval
    try:
        config = ApiGeneratorConfig.from_app_dir(app_dir)
        generator = ApiGenerator(config, logger=logger)
        generator.ensure_config()
        generator.generate_client()
        if logger:
            logger.info("OpenAPI client regenerated successfully")
    except Exception as e:
        if logger:
            logger.error(f"Failed to regenerate OpenAPI client: {e}")
        else:
            console.print(f"[red]Failed to regenerate OpenAPI client: {e}[/red]")

    return True


@with_version
def openapi(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    force: Annotated[
        bool,
        Option(
            "--force", "-f", help="Force regeneration even if schema hasn't changed"
        ),
    ] = False,
) -> None:
    """Generate OpenAPI schema from FastAPI app and generate TypeScript API client."""
    if app_dir is None:
        app_dir = Path.cwd()

    run_openapi(app_dir, force=force)
