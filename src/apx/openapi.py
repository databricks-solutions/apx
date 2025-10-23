"""OpenAPI schema generation and orval client generation utilities."""

import asyncio
import importlib
import json
import subprocess
import sys
from pathlib import Path

from fastapi import FastAPI
from typer import Exit
import watchfiles

from apx.utils import (
    console,
    ensure_dir,
    get_project_metadata,
    in_path,
    progress_spinner,
)


def _ensure_orval_config(app_dir: Path, app_slug: str) -> Path:
    """Ensure orval config file exists with default values."""
    apx_dir = app_dir / ".apx"
    ensure_dir(apx_dir)

    orval_config_path = apx_dir / "orval.config.ts"

    if not orval_config_path.exists():
        # Create default orval config based on vite.config.ts defaults
        orval_config_content = f"""import {{ defineConfig }} from "orval";

export default defineConfig({{
  api: {{
    input: ".apx/openapi.json",
    output: {{
      target: "../src/{app_slug}/ui/lib/api.ts",
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
        orval_config_path.write_text(orval_config_content)
        console.print(
            f"[green]✓[/green] Created orval config at {orval_config_path.relative_to(app_dir)}"
        )

    return orval_config_path


def _generate_openapi_schema(app_dir: Path, app_module_name: str) -> tuple[Path, bool]:
    """Generate OpenAPI schema JSON file.

    Returns:
        Tuple of (output_path, schema_changed) where schema_changed indicates if the schema differs from previous
    """
    # Split the app_name into module path and attribute name (like uvicorn does)
    if ":" not in app_module_name:
        console.print(
            f"[red]❌ Invalid app module format. Expected format: some.package.file:app[/red]"
        )
        raise Exit(code=1)

    module_path, attribute_name = app_module_name.split(":", 1)

    # Import the module
    try:
        # Reload modules to get fresh changes
        base_path = module_path.split(".")[0]
        modules_to_delete = [
            name
            for name in sys.modules.keys()
            if name.startswith(base_path + ".") or name == base_path
        ]
        for mod_name in modules_to_delete:
            del sys.modules[mod_name]

        module = importlib.import_module(module_path)
    except ImportError as e:
        console.print(f"[red]❌ Failed to import module {module_path}: {e}[/red]")
        raise Exit(code=1)

    # Get the app attribute from the module
    try:
        app_instance = getattr(module, attribute_name)
    except AttributeError:
        console.print(
            f"[red]❌ Module {module_path} does not have attribute '{attribute_name}'[/red]"
        )
        raise Exit(code=1)

    if not isinstance(app_instance, FastAPI):
        console.print(
            f"[red]❌ '{attribute_name}' is not a FastAPI app instance.[/red]"
        )
        raise Exit(code=1)

    # Generate OpenAPI spec
    spec = app_instance.openapi()
    new_spec_json = json.dumps(spec, indent=2)

    # Write to .apx/openapi.json
    apx_dir = app_dir / ".apx"
    ensure_dir(apx_dir)
    output_path = apx_dir / "openapi.json"

    # Check if schema has changed
    schema_changed = True
    if output_path.exists():
        existing_spec = output_path.read_text()
        if existing_spec == new_spec_json:
            schema_changed = False

    # Write the new schema if it changed
    if schema_changed:
        output_path.write_text(new_spec_json)

    return output_path, schema_changed


def _run_orval(app_dir: Path, openapi_path: Path, orval_config_path: Path):
    """Run orval to generate the client."""
    result = subprocess.run(
        [
            "bun",
            "x",
            "--bun",
            "orval",
            "-i",
            str(openapi_path.relative_to(app_dir)),
            "-c",
            str(orval_config_path.relative_to(app_dir)),
        ],
        cwd=app_dir,
        capture_output=True,
        text=True,
    )

    if result.returncode != 0:
        console.print("[red]❌ Failed to run orval[/red]")
        if result.stderr:
            console.print(f"[red]{result.stderr}[/red]")
        if result.stdout:
            console.print(f"[red]{result.stdout}[/red]")
        raise Exit(code=1)


def _generate_openapi_and_client(app_dir: Path):
    """Generate OpenAPI schema and run orval to generate client."""
    # Get project metadata
    try:
        with in_path(app_dir):
            metadata = get_project_metadata()
            app_module_name = metadata["app-module"]
            app_slug = metadata["app-slug"]
    except Exception as e:
        console.print(f"[red]❌ Failed to read project metadata: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you're in a valid apx project directory[/yellow]"
        )
        raise Exit(code=1)

    with progress_spinner(
        "📝 Generating OpenAPI schema...", "✅ OpenAPI schema generated"
    ):
        openapi_path, schema_changed = _generate_openapi_schema(
            app_dir, app_module_name
        )

    # Ensure orval config exists
    orval_config_path = _ensure_orval_config(app_dir, app_slug)

    if schema_changed:
        with progress_spinner(
            "🔧 Generating API client with orval...", "✅ API client generated"
        ):
            _run_orval(app_dir, openapi_path, orval_config_path)
        console.print(
            f"[bold green]✨ OpenAPI schema and client generated successfully![/bold green]"
        )
    else:
        console.print("[dim]⏭️  Schema unchanged, skipping orval generation[/dim]")
        console.print(f"[bold green]✨ OpenAPI schema is up to date![/bold green]")


async def _openapi_watch(app_dir: Path):
    """Watch for Python file changes and regenerate OpenAPI schema and client."""
    # Get project metadata
    try:
        with in_path(app_dir):
            metadata = get_project_metadata()
            app_module_name = metadata["app-module"]
            app_slug = metadata["app-slug"]
    except Exception as e:
        console.print(f"[red]❌ Failed to read project metadata: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you're in a valid apx project directory[/yellow]"
        )
        raise Exit(code=1)

    console.print(
        f"[bold cyan]👁️  Watching for changes in {app_dir}/**/*.py[/bold cyan]"
    )
    console.print()

    # Ensure orval config exists (do this once before generating)
    orval_config_path = _ensure_orval_config(app_dir, app_slug)

    # Generate once at startup
    try:
        openapi_path, schema_changed = _generate_openapi_schema(
            app_dir, app_module_name
        )
        if schema_changed:
            _run_orval(app_dir, openapi_path, orval_config_path)
            console.print("[green]✓[/green] Initial generation complete")
        else:
            console.print("[dim]✓[/dim] Schema unchanged, skipping orval")
        console.print()
    except Exception as e:
        console.print(f"[red]❌ Initial generation failed: {e}[/red]")

    # Watch for changes
    try:
        async for changes in watchfiles.awatch(
            app_dir,
            watch_filter=watchfiles.PythonFilter(),
        ):
            console.print(
                f"[yellow]🔄 Detected changes in {len(changes)} file(s), regenerating...[/yellow]"
            )

            try:
                openapi_path, schema_changed = _generate_openapi_schema(
                    app_dir, app_module_name
                )
                if schema_changed:
                    _run_orval(app_dir, openapi_path, orval_config_path)
                    console.print("[green]✓[/green] Regeneration complete")
                else:
                    console.print("[dim]✓[/dim] Schema unchanged, skipping orval")
                console.print()
            except Exception as e:
                console.print(f"[red]❌ Regeneration failed: {e}[/red]")
                console.print()
    except KeyboardInterrupt:
        console.print("\n[dim]Stopped watching for changes.[/dim]")


def run_openapi(app_dir: Path, watch: bool = False):
    """
    Generate OpenAPI schema and orval client.

    Args:
        app_dir: The path to the app directory
        watch: Whether to watch for changes and regenerate
    """
    if watch:
        asyncio.run(_openapi_watch(app_dir))
    else:
        _generate_openapi_and_client(app_dir)
