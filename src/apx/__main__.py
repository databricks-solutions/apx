import asyncio
import importlib
from importlib import resources
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import Annotated, Literal

from dotenv import set_key
from fastapi import FastAPI
import jinja2
from rich import print
from rich.progress import Progress, SpinnerColumn, TextColumn
from typer import Argument, Exit, Option, Typer

from apx._version import version as apx_version
from apx.dev import run_backend
from apx.utils import (
    console,
    ensure_dir,
    get_app_name_from_pyproject,
    is_bun_installed,
    is_uv_installed,
    process_template_directory,
    random_name,
    run_frontend,
    run_subprocess,
    version_callback,
)


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
    app_name: Annotated[
        str | None,
        Option(
            "--name",
            "-n",
            help="The name of the project. Optional, will be generated if not provided",
        ),
    ] = None,
    app_path: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, the app will be created in the current working directory",
        ),
    ] = None,
    profile: Annotated[
        str | None, Option("--profile", "-p", help="The Databricks profile to use")
    ] = None,
    assistant: Annotated[
        Literal["cursor", "vscode", "codex", "claude"] | None,
        Option(
            "--assistant",
            "-a",
            help="The type of assistant to use. If not provided, no assistant rules will be setup.",
        ),
    ] = None,
    db: Annotated[
        bool, Option(help="Whether to add Lakebase support to the project")
    ] = False,
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

    # Normalize app name
    if app_name is None:
        app_name = random_name()
    else:
        app_name = (
            app_name.lower().replace(" ", "_").replace("-", "_").replace(".", "_")
        )
        if not app_name.isalnum():
            print(
                "Invalid app name. Please use only alphanumeric characters and underscores."
            )
            return Exit(code=1)

    if app_path is None:
        app_path = Path.cwd()

    console.print(f"[bold chartreuse1]Welcome to apx 🚀[/bold chartreuse1]")
    console.print(
        f"\n[bold cyan]Initializing app {app_name} in {app_path.resolve()}[/bold cyan]\n"
    )

    # === PHASE 1: Preparing project layout ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("📁 Preparing project layout...", total=None)

        # Ensure app_path exists
        ensure_dir(app_path)

        # Process the entire base template directory
        base_template_dir = templates_dir / "base"
        process_template_directory(base_template_dir, app_path, app_name, jinja2_env)

        # Create dist gitignore
        dist_dir = app_path / "src" / app_name / "__dist__"
        ensure_dir(dist_dir)
        (dist_dir / ".gitignore").write_text("*\n")

        # add a .build directory with .gitignore file
        build_dir = app_path / ".build"
        ensure_dir(build_dir)
        (build_dir / ".gitignore").write_text("*\n")

        # append DATABRICKS_PROFILE to .env if profile is provided
        if profile:
            set_key(app_path / ".env", "DATABRICKS_CONFIG_PROFILE", profile)

        if db:
            # replace databricks.yml.jinja2 with databricks.yml.jinja2 from addons/db
            db_addon = templates_dir / "addons/db"
            process_template_directory(db_addon, app_path, app_name, jinja2_env)

        progress.update(task, description="✅ Project layout prepared", completed=True)

    # === PHASE 2: Installing frontend dependencies ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("📦 Installing frontend dependencies...", total=None)

        # Install bun main dependencies
        run_subprocess(
            [
                "bun",
                "add",
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
        run_subprocess(
            [
                "bun",
                "add",
                "-D",
                "github:renardeinside/apx",
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
            error_msg="Failed to install dev dependencies",
        )

        progress.update(
            task, description="✅ Frontend dependencies installed", completed=True
        )

    # === PHASE 3: Bootstrapping shadcn ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("🎨 Bootstrapping shadcn components...", total=None)

        # Add button component
        run_subprocess(
            ["bun", "x", "shadcn@latest", "add", "button", "--yes"],
            cwd=app_path,
            error_msg="Failed to add button component",
        )

        # Add card component
        run_subprocess(
            ["bun", "x", "shadcn@latest", "add", "card", "--yes"],
            cwd=app_path,
            error_msg="Failed to add card component",
        )

        progress.update(task, description="✅ Shadcn components added", completed=True)

    # === PHASE 4: Initializing git ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("🔧 Initializing git repository...", total=None)

        run_subprocess(
            ["git", "init"],
            cwd=app_path,
            error_msg="Failed to initialize git repository",
        )

        progress.update(
            task, description="✅ Git repository initialized", completed=True
        )

    # === PHASE 5: Syncing project with uv ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("🐍 Setting up project...", total=None)

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

        progress.update(task, description="✅ Project set up", completed=True)

    # === PHASE 6: Build using apx build ===
    build(
        app_path=app_path.resolve(),
        build_path=Path(".build"),
        skip_ui_build=False,
        version=version,
    )

    # === PHASE 7: Setting up assistant rules ===
    if assistant:
        with Progress(
            SpinnerColumn(),
            TextColumn("[progress.description]{task.description}"),
            console=console,
        ) as progress:
            task = progress.add_task("🤖 Setting up assistant rules...", total=None)

            if assistant == "vscode":
                progress.update(task, description="🤖 Copying VSCode instructions...")
                shutil.copytree(
                    templates_dir / "addons/rules/.github/instructions",
                    app_path / ".github/instructions",
                )
            elif assistant == "cursor":
                progress.update(task, description="🤖 Copying Cursor rules...")
                shutil.copytree(
                    templates_dir / "addons/rules/.cursor/rules",
                    app_path / ".cursor/rules",
                )
            else:
                progress.update(
                    task,
                    description=f"⏭️  Skipping assistant rules setup for {assistant}",
                    completed=True,
                )
                console.print(
                    f"""[yellow]⏭️  Skipping assistant rules setup for {assistant}.
                Please add them manually to your editor of choice.[/yellow]"""
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

            progress.update(
                task, description="✅ Assistant rules configured", completed=True
            )

    console.print()
    console.print(
        f"[bold green]✨ Project {app_name} initialized successfully! [/bold green]"
    )
    console.print(
        f"[bold green]🚀 Run `cd {app_path.resolve()}` to get started![/bold green]"
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
    # Clean up the build directory if it exists
    if build_dir.exists():
        shutil.rmtree(build_dir)

    # ensure the build directory exists
    ensure_dir(build_dir)

    # add a .build/.gitignore file
    (build_dir / ".gitignore").write_text("*\n")

    # === PHASE 1: Building UI ===
    if not skip_ui_build:
        with Progress(
            SpinnerColumn(),
            TextColumn("[progress.description]{task.description}"),
            console=console,
        ) as progress:
            task = progress.add_task(f"🎨 Building UI...", total=None)

            result = subprocess.run(
                ["bun", "run", "build"],
                cwd=app_path,
                capture_output=True,
                text=True,
                env=os.environ,
            )

            if result.returncode != 0:
                progress.update(
                    task, description="❌ Failed to build UI", completed=True
                )
                console.print("[red]❌ Failed to build UI[/red]")
                if result.stderr:
                    console.print(f"[red]{result.stderr}[/red]")
                if result.stdout:
                    console.print(f"[red]{result.stdout}[/red]")
                raise Exit(code=1)

            progress.update(task, description="✅ UI built", completed=True)
    else:
        console.print("[yellow]⏭️  Skipping UI build[/yellow]")

    # === PHASE 2: Building Python wheel ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("🐍 Building Python wheel...", total=None)

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

        progress.update(task, description="✅ Python wheel built", completed=True)

    # === PHASE 3: Preparing build directory ===
    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("📦 Preparing build directory...", total=None)

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
        wheel_file_name = f"{wheel_file.stem}+{timestamp}.whl"
        wheel_file.rename(build_dir / wheel_file_name)

        # write requirements.txt with the wheel file name
        requirements_file = build_dir / "requirements.txt"
        requirements_file.write_text(f"{wheel_file_name}\n")

        progress.update(task, description="✅ Build directory prepared", completed=True)


@app.command(name="openapi", help="Generate OpenAPI schema from FastAPI app")
def openapi(
    app_name: str = Argument(
        ..., help="App module name in form of some.package.file:app"
    ),
    output_path: Path = Argument(..., help="The path to the output file"),
    version: bool | None = version_option,
):
    # Split the app_name into module path and attribute name (like uvicorn does)
    if ":" not in app_name:
        print(f"Invalid app name format. Expected format: some.package.file:app")
        return Exit(code=1)

    module_path, attribute_name = app_name.split(":", 1)

    # Import the module
    try:
        module = importlib.import_module(module_path)
    except ImportError as e:
        print(f"Failed to import module {module_path}: {e}")
        return Exit(code=1)

    # Get the app attribute from the module
    try:
        app_instance = getattr(module, attribute_name)
    except AttributeError:
        print(f"Module {module_path} does not have attribute '{attribute_name}'")
        return Exit(code=1)

    if not isinstance(app_instance, FastAPI):
        print(f"'{attribute_name}' is not a FastAPI app instance.")
        return Exit(code=1)

    spec = app_instance.openapi()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(spec, indent=2))


@app.command(name="dev", help="Run development servers for frontend and backend")
def dev(
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
    version: bool | None = version_option,
):
    """
    Run development servers for both frontend and backend concurrently.
    The backend will have a middleware that adds X-Twist: True header.
    """
    # Check prerequisites
    if not is_bun_installed():
        console.print(
            "[red]❌ bun is not installed. Please install bun to continue.[/red]"
        )
        raise Exit(code=1)

    # Get app name from pyproject.toml
    app_name = get_app_name_from_pyproject()
    app_module_name = f"{app_name}.backend.app:app"

    console.print(
        f"[bold chartreuse1]🚀 Starting development servers...[/bold chartreuse1]"
    )
    console.print(f"[cyan]Frontend:[/cyan] http://localhost:{frontend_port}")
    console.print(f"[green]Backend:[/green] http://{backend_host}:{backend_port}")
    console.print()

    async def run_both():
        try:
            await asyncio.gather(
                run_frontend(frontend_port),
                run_backend(
                    Path.cwd(), app_module_name, backend_host, backend_port, obo=obo
                ),
            )
        except KeyboardInterrupt:
            console.print("\n[yellow]⚠️  Shutting down development servers...[/yellow]")

    try:
        asyncio.run(run_both())
    except KeyboardInterrupt:
        console.print("[yellow]✅ Development servers stopped[/yellow]")


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
