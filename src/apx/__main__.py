import asyncio
from importlib import resources
import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import Annotated, Literal

from dotenv import set_key
import jinja2
from rich import print
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.prompt import Confirm, Prompt
from typer import Argument, Exit, Option, Typer

from apx._version import version as apx_version
from apx.dev import DevManager, run_backend
from apx.openapi import run_openapi
from apx.utils import (
    console,
    ensure_dir,
    format_elapsed_ms,
    generate_metadata_file,
    is_bun_installed,
    is_uv_installed,
    list_profiles,
    process_template_directory,
    progress_spinner,
    random_name,
    run_subprocess,
    version_callback,
)


def bun_add(
    packages: list[str],
    cwd: Path,
    dev: bool = False,
    error_msg: str = "Failed to install packages",
) -> None:
    """
    Run bun add command with optional cache directory support.

    Args:
        packages: List of package names to install
        cwd: Current working directory for the command
        dev: Whether to install as dev dependencies (-D flag)
        error_msg: Error message to display if command fails
    """
    cmd = ["bun", "add"]

    # Check if BUN_CACHE_DIR is set and add cache directory flag
    bun_cache_dir = os.environ.get("BUN_CACHE_DIR")
    if bun_cache_dir:
        cache_path = Path(bun_cache_dir).resolve()
        cmd.extend(["--cache-dir", str(cache_path)])

    # Add dev flag if needed
    if dev:
        cmd.append("-D")

    # Add packages
    cmd.extend(packages)
    run_subprocess(cmd, cwd=cwd, error_msg=error_msg)


app = Typer(
    name="apx | Databricks App Toolkit",
)

templates_dir: Path = resources.files("apx").joinpath("templates")  # type: ignore
jinja2_env = jinja2.Environment(loader=jinja2.FileSystemLoader(templates_dir))


version_option = Option(
    None,
    "--version",
    help="Show the version of apx",
    callback=version_callback,
    is_eager=True,
)


@app.callback()
def main(
    version: bool = Option(
        None,
        "--version",
        callback=version_callback,
        is_eager=True,
        help="Show the version and exit.",
    ),
):
    """Project quickstarter CLI."""
    pass


@app.command(name="version", help="Show the version of apx")
def version():
    print(f"apx version: {apx_version}")


@app.command(name="init", help="Initialize a new project")
def init(
    app_path: Annotated[
        Path | None,
        Argument(
            help="The path to the app. Defaults to current working directory",
        ),
    ] = None,
    app_name: Annotated[
        str | None,
        Option(
            "--name",
            "-n",
            help="The name of the project. Will prompt if not provided",
        ),
    ] = None,
    template: Annotated[
        Literal["essential", "stateful"],
        Option(
            "--template",
            "-t",
            help="The template to use. Will prompt if not provided",
        ),
    ] = "essential",
    profile: Annotated[
        str | None,
        Option(
            "--profile",
            "-p",
            help="The Databricks profile to use. Will prompt if not provided",
        ),
    ] = None,
    assistant: Annotated[
        str | None,
        Option(
            "--assistant",
            "-a",
            help="The type of assistant to use (cursor/vscode/codex/claude). Will prompt if not provided",
        ),
    ] = None,
    layout: Annotated[
        Literal["basic", "sidebar"] | None,
        Option(
            "--layout",
            "-l",
            help="The layout to use. Will prompt if not provided",
        ),
    ] = None,
    version: bool | None = version_option,
):
    # Check prerequisites
    if not is_uv_installed():
        print("uv is not installed. Please install uv to continue.")
        return Exit(code=1)
    if not is_bun_installed():
        print("bun is not installed. Please install bun to continue.")
        return Exit(code=1)
    if shutil.which("git") is None:
        print("git is not installed. Please install git to continue.")
        return Exit(code=1)

    # Set default app_path
    if app_path is None:
        app_path = Path.cwd()

    console.print(f"[bold chartreuse1]Welcome to apx 🚀[/bold chartreuse1]\n")

    # Prompt for app name if not provided
    if app_name is None:
        default_name = random_name()
        app_name = Prompt.ask(
            "[cyan]What's the name of your app?[/cyan]",
            default=default_name,
        )

    # Normalize app name: convert to lowercase and replace spaces with dashes
    app_name = app_name.lower().replace(" ", "-").replace("_", "-")
    # Validate that app_name only contains alphanumeric characters and dashes
    if not app_name.replace("-", "").isalnum():
        print(
            "[red]Invalid app name. Please use only alphanumeric characters and dashes.[/red]"
        )
        return Exit(code=1)

    # Create app_slug: internal version with underscores for module names and paths
    app_slug = app_name.replace("-", "_")

    # Prompt for profile if not provided
    if profile is None:
        available_profiles = list_profiles()
        if available_profiles:
            console.print(
                f"[dim]Available Databricks profiles: {', '.join(available_profiles)}[/dim]"
            )
            profile = Prompt.ask(
                "[cyan]Which Databricks profile would you like to use? (leave empty to skip)[/cyan]",
                default="",
                show_default=False,
            )
            if profile == "":
                profile = None
        else:
            console.print("[dim]No Databricks profiles found in ~/.databrickscfg[/dim]")
            if Confirm.ask(
                "[cyan]Would you like to specify a profile name?[/cyan]", default=False
            ):
                profile = Prompt.ask("[cyan]Enter profile name[/cyan]")
            else:
                profile = None

    # Prompt for assistant if not provided
    if assistant is None:
        if Confirm.ask(
            "[cyan]Would you like to set up AI assistant rules?[/cyan]", default=True
        ):
            available_assistants = ["cursor", "vscode", "codex", "claude"]
            assistant = Prompt.ask(
                "[cyan]Which assistant would you like to use?[/cyan]",
                choices=available_assistants,
                default="cursor",
            )

    # Prompt for layout if not provided
    if layout is None:
        available_layouts = ["basic", "sidebar"]
        prompt_layout = Prompt.ask(
            "[cyan]Which layout would you like to use?[/cyan]",
            choices=available_layouts,
            default="sidebar",
        )
        if prompt_layout.lower() not in ["basic", "sidebar"]:
            print("[red]Invalid layout. Please choose from: basic, sidebar.[/red]")
            return Exit(code=1)
        layout = prompt_layout.lower()  # type: ignore

    console.print(
        f"\n[bold cyan]Initializing app {app_name} in {app_path.resolve()}[/bold cyan]\n"
    )

    # === PHASE 1: Preparing project layout ===
    with progress_spinner(
        "📁 Preparing project layout...", "✅ Project layout prepared"
    ):
        # Ensure app_path exists
        ensure_dir(app_path)

        # Process the entire base template directory
        base_template_dir = templates_dir / "base"
        process_template_directory(
            base_template_dir, app_path, app_name, app_slug, jinja2_env
        )

        # Create dist gitignore
        dist_dir = app_path / "src" / app_slug / "__dist__"
        ensure_dir(dist_dir)
        (dist_dir / ".gitignore").write_text("*\n")

        # add a .build directory with .gitignore file
        build_dir = app_path / ".build"
        ensure_dir(build_dir)
        (build_dir / ".gitignore").write_text("*\n")

        if template == "stateful":
            # replace databricks.yml.jinja2 with databricks.yml.jinja2 from addons/stateful
            stateful_addon = templates_dir / "addons/stateful"
            process_template_directory(
                stateful_addon, app_path, app_name, app_slug, jinja2_env
            )

        # append DATABRICKS_CONFIG_PROFILE to .env if profile is provided
        if profile:
            set_key(app_path / ".env", "DATABRICKS_CONFIG_PROFILE", profile)

        if layout == "sidebar":
            # replace src/base/ui/routes/__root.tsx with src/base/ui/routes/__root.tsx from addons/sidebar
            sidebar_addon = templates_dir / "addons/sidebar"
            process_template_directory(
                sidebar_addon, app_path, app_name, app_slug, jinja2_env
            )

    # === PHASE 2: Installing frontend dependencies ===
    with progress_spinner(
        "📦 Installing frontend dependencies...", "✅ Frontend dependencies installed"
    ):
        # Install bun main dependencies
        bun_add(
            [
                "react-error-boundary",
                "axios",
                "react",
                "react-dom",
                "class-variance-authority",
                "clsx",
                "tailwind-merge",
                "lucide-react",
                "tw-animate-css",
                "@tanstack/react-router",
                "@tanstack/react-query",
                "sonner",
            ],
            cwd=app_path,
            error_msg="Failed to install main dependencies",
        )

        # Install bun dev dependencies
        bun_add(
            [
                "smol-toml",
                "orval",
                "vite",
                "typescript",
                "@types/node",
                "@types/react",
                "@types/react-dom",
                "@vitejs/plugin-react",
                "@tailwindcss/vite",
                "@tanstack/router-plugin",
                "@tanstack/react-router-devtools",
            ],
            cwd=app_path,
            dev=True,
            error_msg="Failed to install dev dependencies",
        )

    # === PHASE 3: Bootstrapping shadcn ===
    with progress_spinner(
        "🎨 Bootstrapping shadcn components...", "✅ Shadcn components added"
    ):
        # Add button component
        run_subprocess(
            ["bun", "x", "--bun", "shadcn@latest", "add", "button", "card", "--yes"],
            cwd=app_path,
            error_msg="Failed to add button component",
        )

        # add bubble background component
        run_subprocess(
            [
                "bun",
                "x",
                "--bun",
                "shadcn@latest",
                "add",
                "@animate-ui/components-backgrounds-bubble",
                "-p",
                f"src/{app_slug}/ui/components/backgrounds/bubble.tsx",
                "--yes",
            ],
            cwd=app_path,
            error_msg="Failed to add bubble background component",
        )

        if layout == "sidebar":
            # install necessary components for sidebar layout
            run_subprocess(
                [
                    "bun",
                    "x",
                    "--bun",
                    "shadcn@latest",
                    "add",
                    "avatar",
                    "sidebar",
                    "separator",
                    "skeleton",
                    "badge",
                    "--yes",
                ],
                cwd=app_path,
                error_msg="Failed to add avatar and sidebar components",
            )

    # === PHASE 4: Initializing git ===
    with progress_spinner(
        "🔧 Initializing git repository...", "✅ Git repository initialized"
    ):
        run_subprocess(
            ["git", "init"],
            cwd=app_path,
            error_msg="Failed to initialize git repository",
        )
        run_subprocess(
            ["git", "add", "."],
            cwd=app_path,
            error_msg="Failed to add files to git repository",
        )
        run_subprocess(
            ["git", "commit", "-m", "init"],
            cwd=app_path,
            error_msg="Failed to commit files to git repository",
        )

    # === PHASE 5: Syncing project with uv ===
    phase_start = time.perf_counter()
    with Progress(
        SpinnerColumn(finished_text=""),
        TextColumn("[progress.description]{task.description}"),
        console=console,
        transient=True,
    ) as progress:
        task = progress.add_task("🐍 Setting up project...", total=None)

        # Generate the _metadata.py file
        generate_metadata_file(app_path)

        # Start uv sync in background
        proc = subprocess.Popen(
            ["uv", "sync"],
            cwd=app_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Monitor progress for up to 10 seconds
        start_time = time.time()
        warning_shown = False

        while proc.poll() is None:
            elapsed = time.time() - start_time
            if elapsed >= 10 and not warning_shown:
                progress.update(
                    task,
                    description="🐍 Setting up project (taking longer than expected)...",
                )
                warning_shown = True
            time.sleep(0.1)

        # Get the result
        stdout, stderr = proc.communicate()

        if proc.returncode != 0:
            console.print("[red]❌ Failed to set up project[/red]")
            if stderr:
                console.print(f"[red]{stderr}[/red]")
            if stdout:
                console.print(f"[red]{stdout}[/red]")
            raise Exit(code=1)

    console.print(f"✅ Project set up ({format_elapsed_ms(phase_start)})")

    # === PHASE 6: Build using apx build ===

    with progress_spinner("🔧 Building project...", "✅ Project built"):
        subprocess.run(
            ["uv", "run", "apx", "build"],
            cwd=app_path,
            capture_output=True,
            text=True,
            env=os.environ,
        )

    # === PHASE 7: Setting up assistant rules ===
    if assistant:
        phase_start = time.perf_counter()
        with Progress(
            SpinnerColumn(finished_text=""),
            TextColumn("[progress.description]{task.description}"),
            console=console,
            transient=True,
        ) as progress:
            task = progress.add_task("🤖 Setting up assistant rules...", total=None)

            if assistant == "vscode":
                progress.update(task, description="🤖 Copying VSCode instructions...")
                rules_addon = templates_dir / "addons/vscode"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )
            elif assistant == "cursor":
                progress.update(task, description="🤖 Copying Cursor rules...")
                rules_addon = templates_dir / "addons/cursor"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )
            else:
                console.print(
                    f"""[yellow]⏭️  Skipping assistant rules setup for {assistant}.
                Please add them manually to your editor of choice.[/yellow]"""
                )
                console.print(
                    f"⏭️  Skipping assistant rules setup for {assistant} ({format_elapsed_ms(phase_start)})"
                )

            # install shadcn mcp via CLI
            progress.update(task, description="🤖 Installing shadcn MCP...")
            run_subprocess(
                [
                    "bun",
                    "x",
                    "--bun",
                    "shadcn@latest",
                    "mcp",
                    "init",
                    "--client",
                    assistant,
                ],
                cwd=app_path,
                error_msg="Failed to install shadcn mcp",
            )

        console.print(
            f"✅ MCP installed and assistant rules configured ({format_elapsed_ms(phase_start)})"
        )

    console.print()
    console.print(
        f"[bold green]✨ Project {app_name} initialized successfully! [/bold green]"
    )
    console.print(
        f"[bold green]🚀 Run `cd {app_path.resolve()} && uv run apx dev` to get started![/bold green]"
    )


@app.command(name="build", help="Build the project (frontend + Python wheel)")
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
    version: bool | None = version_option,
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
    version: bool | None = version_option,
):
    """Generate OpenAPI schema from FastAPI app and run orval to generate client."""
    if app_dir is None:
        app_dir = Path.cwd()

    run_openapi(app_dir, watch=watch)


# === Dev Command Group ===

dev_app = Typer(name="dev", help="Manage development servers")
app.add_typer(dev_app, name="dev")


@app.command(
    name="_run-frontend-detached",
    hidden=True,
    help="Internal: Run frontend in detached mode",
)
def _run_frontend_detached(
    app_dir: Path = Argument(..., help="App directory"),
    app_id: str = Argument(..., help="Application ID"),
    frontend_port: int = Argument(..., help="Frontend port"),
):
    """Internal command to run frontend server with file logging. Not meant for direct use."""
    from apx.dev import run_frontend_with_logging

    asyncio.run(run_frontend_with_logging(app_dir, app_id, frontend_port))


@app.command(
    name="_run-backend-detached",
    hidden=True,
    help="Internal: Run backend in detached mode",
)
def _run_backend_detached(
    app_dir: Path = Argument(..., help="App directory"),
    app_id: str = Argument(..., help="Application ID"),
    app_module_name: str = Argument(..., help="App module name"),
    backend_host: str = Argument(..., help="Backend host"),
    backend_port: int = Argument(..., help="Backend port"),
    obo: bool = Argument(..., help="Enable OBO"),
):
    """Internal command to run backend server with file logging. Not meant for direct use."""
    from apx.dev import get_log_dir

    log_dir = get_log_dir(app_id)
    log_file = log_dir / "backend.log"

    asyncio.run(
        run_backend(
            app_dir,
            app_module_name,
            backend_host,
            backend_port,
            obo=obo,
            log_file=log_file,
        )
    )


@app.command(
    name="_run-openapi-detached",
    hidden=True,
    help="Internal: Run OpenAPI watcher in detached mode",
)
def _run_openapi_detached(
    app_dir: Path = Argument(..., help="App directory"),
    app_id: str = Argument(..., help="Application ID"),
):
    """Internal command to run OpenAPI watcher with file logging. Not meant for direct use."""
    from apx.dev import run_openapi_with_logging

    asyncio.run(run_openapi_with_logging(app_dir, app_id))


@dev_app.command(name="start", help="Start development servers in detached mode")
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

    # Use DevManager to start servers
    manager = DevManager(app_dir)
    manager.start(
        frontend_port=frontend_port,
        backend_port=backend_port,
        backend_host=backend_host,
        obo=obo,
        openapi=openapi,
    )


@dev_app.command(name="status", help="Check the status of development servers")
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


@dev_app.command(name="logs", help="Retrieve and display logs from the database")
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
):
    """Retrieve and display logs from the log files."""
    if app_dir is None:
        app_dir = Path.cwd()

    # Use DevManager to retrieve logs
    manager = DevManager(app_dir)
    logs = manager.get_logs(
        duration_seconds=duration,
        ui_only=ui,
        backend_only=backend,
        openapi_only=openapi,
    )

    if not logs:
        console.print("[yellow]No logs found.[/yellow]")
        return

    # Display all logs
    for log in logs:
        manager._print_log_entry(log)

    console.print(f"\n[dim]Showing {len(logs)} log entries[/dim]")


@dev_app.command(name="tail", help="Tail logs continuously from log files")
def dev_tail(
    app_dir: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, current working directory will be used"
        ),
    ] = None,
    duration: Annotated[
        int | None,
        Option("--duration", "-d", help="Initially show logs from the last N seconds"),
    ] = None,
    timeout: Annotated[
        int | None,
        Option(
            "--timeout", "-t", help="Stop tailing after N seconds (None = indefinite)"
        ),
    ] = None,
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
):
    """Tail logs continuously from log files."""
    if app_dir is None:
        app_dir = Path.cwd()

    # Use DevManager to tail logs
    manager = DevManager(app_dir)
    manager.tail_logs(
        duration_seconds=duration,
        ui_only=ui,
        backend_only=backend,
        openapi_only=openapi,
        timeout_seconds=timeout,
    )


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
