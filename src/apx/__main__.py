import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import Annotated, Literal

from dotenv import load_dotenv
from rich import print
from typer import Argument, Exit, Option, Typer

from databricks.sdk import WorkspaceClient

from apx._version import version as apx_version
from apx.cli.init import init as init_command
from apx.cli.version import with_version
from apx.dev import (
    DevManager,
    validate_databricks_credentials,
    delete_token_from_keyring,
    save_token_id,
    suppress_output_and_logs,
)
from apx.openapi import run_openapi
from apx import __version__ as apx_lib_version
from apx.utils import (
    console,
    ensure_dir,
    format_elapsed_ms,
    generate_metadata_file,
    is_bun_installed,
    progress_spinner,
)


app = Typer(
    name="apx | Databricks App Toolkit",
)


@app.callback()
@with_version
def main():
    """Project quickstarter CLI."""
    pass


@app.command(name="version", help="Show the version of apx")
def version():
    print(f"apx version: {apx_version}")


app.command(name="init", help="Initialize a new project")(init_command)


@app.command(name="build", help="Build the project (frontend + Python wheel)")
@with_version
def build(
    app_path: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used",
        ),
    ] = None,
    build_path: Annotated[
        Path,
        Option(
            help="Path to the build directory where artifacts will be placed, relative to the app path"
        ),
    ] = Path(".build"),
    skip_ui_build: Annotated[bool, Option(help="Skip the UI build step")] = False,
):
    """
    Build the project by:
    1. Running bun run build (unless skipped)
    2. Running uv build --wheel
    3. Preparing the .build folder with artifacts and requirements.txt
    """
    if app_path is None:
        app_path = Path.cwd()

    build_dir = app_path / build_path

    console.print(f"🔧 Building project in {app_path.resolve()}")

    # ensure .apx directory exists
    apx_dir = app_path / ".apx"
    ensure_dir(apx_dir)

    start_time_perf = time.perf_counter()
    # Clean up the build directory if it exists
    if build_dir.exists():
        shutil.rmtree(build_dir)

    # ensure the build directory exists
    ensure_dir(build_dir)

    # add a .build/.gitignore file
    (build_dir / ".gitignore").write_text("*\n")

    # Generate the _metadata.py file
    generate_metadata_file(app_path)

    # Generate the openapi schema and orval client
    run_openapi(app_path, watch=False)

    # === PHASE 1: Building UI ===
    if not skip_ui_build:
        with progress_spinner("🎨 Building UI...", "✅ UI built"):
            result = subprocess.run(
                ["bun", "run", "build"],
                cwd=app_path,
                capture_output=True,
                text=True,
                env=os.environ,
            )

            if result.returncode != 0:
                console.print("[red]❌ Failed to build UI[/red]")
                if result.stderr:
                    console.print(f"[red]{result.stderr}[/red]")
                if result.stdout:
                    console.print(f"[red]{result.stdout}[/red]")
                raise Exit(code=1)
    else:
        console.print("[yellow]⏭️  Skipping UI build[/yellow]")

    # === PHASE 2: Building Python wheel ===
    with progress_spinner("🐍 Building Python wheel...", "✅ Python wheel built"):
        result = subprocess.run(
            ["uv", "build", "--wheel", "--out-dir", str(build_path)],
            cwd=app_path,
            capture_output=True,
            text=True,
            env=os.environ,
        )

        if result.returncode != 0:
            console.print("[red]❌ Failed to build Python wheel[/red]")
            if result.stderr:
                console.print(f"[red]{result.stderr}[/red]")
            if result.stdout:
                console.print(f"[red]{result.stdout}[/red]")
            raise Exit(code=1)

    # === PHASE 3: Preparing build directory ===

    # Copy app.yml or app.yaml if it exists
    for app_file_name in ["app.yml", "app.yaml"]:
        app_file = app_path / app_file_name
        if app_file.exists():
            ensure_dir(build_dir)
            shutil.copy(app_file, build_dir / app_file_name)
            break

    wheel_file = list(build_dir.glob("*.whl"))[0]

    if not wheel_file:
        console.print("[red]❌ No wheel file found in build directory[/red]")
        raise Exit(code=1)

    # postfix the wheel file name with the current UTC timestamp
    # Use + separator for local version identifier (PEP 440 compliant)
    timestamp = time.strftime("%Y%m%d%H%M%S")
    # add ".post{timestamp}" before -py3
    stemmed = wheel_file.stem.replace("-py3", f".post{timestamp}-py3")
    wheel_file_name = f"{stemmed}.whl"
    wheel_file.rename(build_dir / wheel_file_name)

    # write requirements.txt with the wheel file name
    requirements_file = build_dir / "requirements.txt"
    requirements_file.write_text(f"{wheel_file_name}\n")

    console.print(f"✅ Full build completed in ({format_elapsed_ms(start_time_perf)})")


@app.command(name="openapi", help="Generate OpenAPI schema and orval client")
@with_version
def openapi(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    watch: Annotated[
        bool,
        Option("--watch", "-w", help="Watch for changes and regenerate"),
    ] = False,
    force: Annotated[
        bool,
        Option(
            "--force", "-f", help="Force regeneration even if schema hasn't changed"
        ),
    ] = False,
):
    """Generate OpenAPI schema from FastAPI app and run orval to generate client."""
    if app_dir is None:
        app_dir = Path.cwd()

    run_openapi(app_dir, watch=watch, force=force)


# === Dev Command Group ===

dev_app = Typer(name="dev", help="Manage development servers")
app.add_typer(dev_app, name="dev")


@app.command(
    name="_run-dev-server",
    hidden=True,
    help="Internal: Run dev server in detached mode",
)
def _run_dev_server(
    app_dir: Path = Argument(..., help="App directory"),
    dev_server_port: int = Argument(..., help="Dev server port"),
    frontend_port: int = Argument(..., help="Frontend port"),
    backend_port: int = Argument(..., help="Backend port"),
    backend_host: str = Argument(..., help="Backend host"),
    obo: str = Argument(..., help="Enable OBO (true/false)"),
    openapi: str = Argument(..., help="Enable OpenAPI (true/false)"),
    max_retries: int = Argument(10, help="Maximum retry attempts"),
):
    """Internal command to run dev server. Not meant for direct use."""
    from apx.dev_server import run_dev_server

    run_dev_server(app_dir, dev_server_port)


@dev_app.command(name="start", help="Start development servers in detached mode")
@with_version
def dev_start(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    frontend_port: Annotated[
        int, Option(help="Port for the frontend development server")
    ] = 5173,
    backend_port: Annotated[int, Option(help="Port for the backend server")] = 8000,
    backend_host: Annotated[
        str, Option(help="Host for the backend server")
    ] = "0.0.0.0",
    obo: Annotated[
        bool, Option(help="Whether to add On-Behalf-Of header to the backend server")
    ] = True,
    openapi: Annotated[
        bool, Option(help="Whether to start OpenAPI watcher process")
    ] = True,
    max_retries: Annotated[
        int, Option(help="Maximum number of retry attempts for processes")
    ] = 10,
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

    # Validate Databricks credentials if OBO is enabled
    if obo:
        console.print("[cyan]🔐 Validating Databricks credentials...[/cyan]")

        dotenv_path = app_dir / ".env"
        if dotenv_path.exists():
            console.print(f"🔍 Loading .env file from {dotenv_path.resolve()}")
            load_dotenv(dotenv_path)

        try:
            with suppress_output_and_logs():
                ws = WorkspaceClient(product="apx/dev", product_version=apx_lib_version)
        except Exception as e:
            console.print(
                f"[red]❌ Failed to initialize Databricks client for OBO token generation: {e}[/red]"
            )
            console.print(
                "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
            )
            raise Exit(code=1)

        if not validate_databricks_credentials(ws):
            # Clear any cached OBO tokens since they were created with invalid credentials
            keyring_id = str(app_dir.resolve())
            console.print(
                "[yellow]⚠️  Invalid Databricks credentials detected. Clearing cached tokens...[/yellow]"
            )
            delete_token_from_keyring(keyring_id)
            save_token_id(app_dir, token_id="")  # Clear the token_id

        console.print("[green]✓[/green] Databricks credentials validated")
        console.print()

    # Use DevManager to start servers
    manager = DevManager(app_dir)
    manager.start(
        frontend_port=frontend_port,
        backend_port=backend_port,
        backend_host=backend_host,
        obo=obo,
        openapi=openapi,
        max_retries=max_retries,
    )

    # If watch mode is enabled, stream logs until Ctrl+C
    if watch:
        console.print()
        console.print(
            "[bold cyan]📡 Streaming logs... Press Ctrl+C to stop servers[/bold cyan]"
        )
        console.print()
        # stream_logs catches KeyboardInterrupt internally, so it returns normally
        # After it returns (for any reason), we should stop the servers
        manager.stream_logs(
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
        manager.stop()


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

    # Use DevManager to check status
    manager = DevManager(app_dir)
    manager.status()


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

    # Use DevManager to stop servers
    manager = DevManager(app_dir)
    manager.stop()


@dev_app.command(name="restart", help="Restart development servers")
def dev_restart(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    watch: Annotated[
        bool,
        Option(
            "--watch",
            "-w",
            help="Tail logs after restart until Ctrl+C, then stop all servers",
        ),
    ] = False,
):
    """Restart development servers using the dev server API."""
    if app_dir is None:
        app_dir = Path.cwd()

    # Use DevManager to restart servers
    manager = DevManager(app_dir)

    # Get config
    config = manager._get_or_create_config()

    if not config.dev_server_pid or not config.dev_server_port:
        console.print("[yellow]No development server found.[/yellow]")
        console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
        raise Exit(code=1)

    if not manager._is_process_running(config.dev_server_pid):
        console.print("[red]Development server is not running.[/red]")
        console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
        raise Exit(code=1)

    console.print("[bold yellow]🔄 Restarting development servers...[/bold yellow]")

    # Send restart request to dev server
    import requests

    try:
        response = requests.post(
            f"http://localhost:{config.dev_server_port}/actions/restart",
            timeout=10,
        )

        if response.status_code == 200:
            console.print(
                "[bold green]✨ Development servers restarted successfully![/bold green]"
            )
        else:
            console.print(
                f"[yellow]⚠️  Warning: Dev server responded with status {response.status_code}[/yellow]"
            )
    except Exception as e:
        console.print(f"[red]❌ Could not connect to dev server: {e}[/red]")
        raise Exit(code=1)

    # If watch mode is enabled, stream logs until Ctrl+C
    if watch:
        console.print()
        console.print(
            "[bold cyan]📡 Streaming logs... Press Ctrl+C to stop servers[/bold cyan]"
        )
        console.print()
        # stream_logs catches KeyboardInterrupt internally, so it returns normally
        # After it returns (for any reason), we should stop the servers
        manager.stream_logs(
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
        manager.stop()


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
    raw: Annotated[
        bool,
        Option("--raw", help="Show raw log output without prefix formatting"),
    ] = False,
):
    """Display logs from development servers. Use -f/--follow to stream continuously."""
    if app_dir is None:
        app_dir = Path.cwd()

    # Use DevManager to stream logs
    manager = DevManager(app_dir)
    manager.stream_logs(
        duration_seconds=duration,
        ui_only=ui,
        backend_only=backend,
        openapi_only=openapi,
        app_only=app,
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
    if result.returncode != 0:
        console.print("[red]❌ Pyright found errors, errors provided below[/]")
        for line in result.stdout.splitlines():
            console.print(f"[red]{line}[/red]")
        raise Exit(code=1)
    else:
        console.print("[green]✅ Pyright found no errors[/green]")


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
