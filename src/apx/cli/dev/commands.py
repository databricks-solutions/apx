"""Dev commands for the apx CLI."""

import os
import re
import subprocess
from pathlib import Path
from typing import Annotated

from dotenv import load_dotenv
from typer import Argument, Exit, Option, Typer

from apx.cli.dev.apply import apply as apply_command
from apx.cli.dev.core import (
    DevCore,
    validate_databricks_credentials,
    delete_token_from_keyring,
    save_token_id,
)
from apx.cli.version import with_version
from apx.models import DevServerConfig
from apx.utils import (
    console,
    is_bun_installed,
)


# Create the dev app (subcommand group)
dev_app = Typer(name="dev", help="Manage development servers")


@dev_app.command(
    name="_run_server",
    hidden=True,
    help="Internal: Run dev server in detached mode",
)
def _run_server(
    app_dir: Path = Argument(..., help="App directory"),
    dev_server_port: int = Argument(..., help="Dev server port"),
    frontend_port: int = Argument(..., help="Frontend port"),
    backend_port: int = Argument(..., help="Backend port"),
    host: str = Argument(..., help="Host for servers"),
    api_prefix: str = Argument(..., help="API prefix (e.g., /api)"),
    obo: str = Argument(..., help="Enable OBO (true/false)"),
    openapi: str = Argument(..., help="Enable OpenAPI (true/false)"),
    max_retries: int = Argument(10, help="Maximum retry attempts"),
):
    """Internal command to run dev server. Not meant for direct use."""
    from apx.cli.dev.server import run_dev_server

    run_dev_server(app_dir, dev_server_port, host)


@dev_app.command(
    name="_run_backend",
    hidden=True,
    help="Internal: Run backend server with hot reload",
)
def _run_backend(
    app_dir: Path = Argument(..., help="App directory"),
    backend_port: int = Argument(..., help="Backend port"),
    host: str = Argument(..., help="Host for backend server"),
    obo: str = Argument(..., help="Enable OBO (true/false)"),
):
    """Internal command to run backend server. Not meant for direct use.

    This command is spawned as a subprocess by the dev server.
    It handles hot-reload internally using watchfiles.
    """
    import os

    from apx.cli.dev.core import run_backend_server
    from apx.models import ProjectMetadata

    # Change to app directory so ProjectMetadata.read() works correctly
    os.chdir(app_dir)

    # Get app module name from pyproject.toml
    metadata: ProjectMetadata = ProjectMetadata.read()
    app_module_name: str = metadata.app_module

    # Parse OBO flag
    obo_enabled = obo.lower() == "true"

    # Run the backend server (blocking with hot reload)
    run_backend_server(
        cwd=app_dir,
        app_module_name=app_module_name,
        host=host,
        backend_port=backend_port,
        obo=obo_enabled,
    )


@dev_app.command(name="start", help="Start development servers in detached mode")
@with_version
def dev_start(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    host: Annotated[
        str | None, Option(help="Host for dev, frontend, and backend servers")
    ] = None,
    api_prefix: Annotated[
        str | None, Option("--api-prefix", help="URL prefix for API routes")
    ] = None,
    obo: Annotated[
        bool | None,
        Option(help="Whether to add On-Behalf-Of header to the backend server"),
    ] = None,
    openapi: Annotated[
        bool | None, Option(help="Whether to start OpenAPI watcher process")
    ] = None,
    max_retries: Annotated[
        int | None, Option(help="Maximum number of retry attempts for processes")
    ] = None,
    watch: Annotated[
        bool,
        Option(
            "--watch",
            "-w",
            help="Start servers and tail logs until Ctrl+C, then stop all servers",
        ),
    ] = False,
):
    """Start development servers in detached mode."""
    # Check prerequisites
    if not is_bun_installed():
        console.print(
            "[red]❌ bun is not installed. Please install bun to continue.[/red]"
        )
        raise Exit(code=1)

    if app_dir is None:
        app_dir = Path.cwd()

    # Build config from CLI options (use defaults from DevServerConfig if not specified)
    default_config = DevServerConfig()
    config = DevServerConfig(
        host=host if host is not None else default_config.host,
        api_prefix=api_prefix if api_prefix is not None else default_config.api_prefix,
        obo=obo if obo is not None else default_config.obo,
        openapi=openapi if openapi is not None else default_config.openapi,
        max_retries=max_retries
        if max_retries is not None
        else default_config.max_retries,
        watch=watch,
    )

    # Validate Databricks credentials if OBO is enabled
    if config.obo:
        console.print("[cyan]🔐 Validating Databricks credentials...[/cyan]")

        dotenv_path = app_dir / ".env"
        if dotenv_path.exists():
            console.print(f"🔍 Loading .env file from {dotenv_path.resolve()}")
            load_dotenv(dotenv_path)

        try:
            if not validate_databricks_credentials():
                # Clear any cached OBO tokens since they were created with invalid credentials
                keyring_id = str(app_dir.resolve())
                console.print(
                    "[yellow]⚠️  Invalid Databricks credentials detected. Clearing cached tokens...[/yellow]"
                )
                delete_token_from_keyring(keyring_id)
                save_token_id(app_dir, token_id="")  # Clear the token_id

                # Raise error and don't start the server
                console.print(
                    "[red]❌ Failed to authenticate with Databricks. Cannot start server with --obo flag.[/red]"
                )
                console.print(
                    "[yellow]💡 Please check your Databricks credentials and try again.[/yellow]"
                )

                # If using a specific profile, show re-authentication command
                profile_name = os.environ.get("DATABRICKS_CONFIG_PROFILE")
                if profile_name:
                    console.print()
                    console.print(
                        "[cyan]Use Databricks CLI to re-authenticate with identified profile:[/cyan]"
                    )
                    console.print()
                    console.print(
                        f"  [bold]> databricks auth login -p {profile_name}[/bold]"
                    )
                    console.print()

                raise Exit(code=1)
        except Exit:
            raise
        except Exception as e:
            console.print(
                f"[red]❌ Failed to validate Databricks credentials: {e}[/red]"
            )
            console.print(
                "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
            )
            raise Exit(code=1)

        console.print("[green]✓[/green] Databricks credentials validated")
        console.print()

    # Use DevCore to start servers with the config
    core = DevCore(app_dir)
    core.start(config=config)

    # If watch mode is enabled, stream logs until Ctrl+C
    if watch:
        console.print()
        console.print(
            "[bold cyan]📡 Streaming logs... Press Ctrl+C to stop servers[/bold cyan]"
        )
        console.print()
        # stream_logs catches KeyboardInterrupt internally, so it returns normally
        # After it returns (for any reason), we should stop the servers
        core.stream_logs(
            duration_seconds=None,
            ui_only=False,
            backend_only=False,
            openapi_only=False,
            app_only=False,
            raw_output=False,
            follow=True,
        )
        console.print()
        console.print("[bold yellow]🛑 Stopping development servers...[/bold yellow]")
        core.stop()


@dev_app.command(name="status", help="Check the status of development servers")
@with_version
def dev_status(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
):
    """Check the status of development servers."""
    if app_dir is None:
        app_dir = Path.cwd()

    core = DevCore(app_dir)
    core.status()


@dev_app.command(name="stop", help="Stop development servers")
@with_version
def dev_stop(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
):
    """Stop development servers."""
    if app_dir is None:
        app_dir = Path.cwd()

    core = DevCore(app_dir)
    core.stop()


@dev_app.command(name="restart", help="Restart development servers")
def dev_restart(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
):
    """Restart development servers using the dev server API."""
    if app_dir is None:
        app_dir = Path.cwd()

    core = DevCore(app_dir)
    core.restart()


@dev_app.command(name="logs", help="Display logs from development servers")
def dev_logs(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    duration: Annotated[
        int | None,
        Option(
            "--duration",
            "-d",
            help="Show logs from the last N seconds (None = all logs)",
        ),
    ] = None,
    follow: Annotated[
        bool,
        Option(
            "--follow",
            "-f",
            help="Follow log output (like tail -f). Streams new logs continuously.",
        ),
    ] = False,
    ui: Annotated[
        bool,
        Option("--ui", help="Show only frontend/UI logs"),
    ] = False,
    backend: Annotated[
        bool,
        Option("--backend", help="Show only backend logs"),
    ] = False,
    openapi: Annotated[
        bool,
        Option("--openapi", help="Show only OpenAPI logs"),
    ] = False,
    app: Annotated[
        bool,
        Option("--app", help="Show only application logs (from your app code)"),
    ] = False,
    system: Annotated[
        bool,
        Option(
            "--system",
            help="Show only system logs from the apx dev server ([apx])",
        ),
    ] = False,
    raw: Annotated[
        bool,
        Option("--raw", help="Show raw log output without prefix formatting"),
    ] = False,
):
    """Display logs from development servers. Use -f/--follow to stream continuously."""
    if app_dir is None:
        app_dir = Path.cwd()

    core = DevCore(app_dir)
    core.stream_logs(
        duration_seconds=duration,
        ui_only=ui,
        backend_only=backend,
        openapi_only=openapi,
        app_only=app,
        system_only=system,
        raw_output=raw,
        follow=follow,
    )


@dev_app.command(name="check", help="Check the project code for errors")
@with_version
def dev_check(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
):
    """Check the project code for errors."""
    if app_dir is None:
        app_dir = Path.cwd()

    console.print(
        "[cyan]🔍 Checking project code for error, starting with TypeScript...[/cyan]"
    )
    console.print("[dim]Running 'bun run tsc -b --incremental'[/dim]")

    # run tsc to check for errors
    result = subprocess.run(
        ["bun", "run", "tsc", "-b", "--incremental"],
        cwd=app_dir,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        console.print("[red]❌ TypeScript compilation failed, errors provided below[/]")
        for line in result.stdout.splitlines():
            console.print(f"[red]{line}[/red]")
        raise Exit(code=1)

    console.print("[green]✅ TypeScript compilation succeeded[/green]")
    console.print()

    console.print("[cyan]🔍 Checking Python code for errors...[/cyan]")
    console.print("[dim]Running 'uv run basedpyright --level error'[/dim]")

    # run pyright to check for errors
    result = subprocess.run(
        ["uv", "run", "basedpyright", "--level", "error"],
        cwd=app_dir,
        capture_output=True,
        text=True,
    )

    # basedpyright may return non-zero exit code even for warnings only
    # we need to parse the output to check for actual errors
    has_errors = False
    if result.returncode != 0:
        # look for the summary line like "X errors, Y warnings, Z notes"
        for line in result.stdout.splitlines():
            match = re.search(r"(\d+)\s+errors?", line)
            if match and int(match.group(1)) > 0:
                has_errors = True
                break

    if has_errors:
        console.print("[red]❌ Pyright found errors, errors provided below[/]")
        for line in result.stdout.splitlines():
            console.print(f"[red]{line}[/red]")
        raise Exit(code=1)
    else:
        console.print("[green]✅ Pyright found no errors[/green]")


@dev_app.command(name="mcp", help="Start MCP server for development server management")
def dev_mcp():
    """Start MCP server that provides tools for managing development servers.

    The MCP server runs over stdio and provides the following tools:
    - start: Start development servers (frontend, backend, OpenAPI watcher)
    - restart: Restart all development servers
    - stop: Stop all development servers
    - status: Get the status of all development servers
    - get_metadata: Get project metadata from pyproject.toml

    This command should be run from the project root directory.
    """
    from apx.mcp import run_mcp_server

    run_mcp_server()


# Register apply command
dev_app.command(name="apply", help="Apply an addon to an existing project")(
    apply_command
)
