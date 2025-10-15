import asyncio
import importlib
import json
import os
import shutil
import subprocess
import time
import tomllib
from importlib import resources
from pathlib import Path
import random
from typing import Annotated

import jinja2
from fastapi import FastAPI
from rich import print
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn
from typer import Argument, Exit, Typer, Option

from apx._version import version as apx_version
from apx.dev import run_backend

console = Console()


def version_callback(value: bool):
    if value:
        print(f"apx version: {apx_version}")
        raise Exit(code=0)


app = Typer(
    name="apx | project quickstarter",
)

templates_dir: Path = resources.files("apx").joinpath("templates")  # type: ignore
jinja2_env = jinja2.Environment(loader=jinja2.FileSystemLoader(templates_dir))


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


def is_uv_installed() -> bool:
    """Check if uv is installed on the system."""
    return shutil.which("uv") is not None


def is_bun_installed() -> bool:
    """Check if bun is installed on the system."""
    return shutil.which("bun") is not None


def random_name():
    # docker-style random name
    adjectives = [
        "fast",
        "simple",
        "clean",
        "elegant",
        "modern",
        "cool",
        "awesome",
        "brave",
        "bold",
        "creative",
        "curious",
        "dynamic",
        "energetic",
        "fantastic",
        "giant",
    ]

    animals = [
        "lion",
        "tiger",
        "bear",
        "wolf",
        "fox",
        "dog",
        "cat",
        "bird",
        "fish",
        "horse",
        "rabbit",
        "turtle",
        "whale",
        "dolphin",
        "shark",
        "octopus",
    ]

    return f"{random.choice(adjectives)}_{random.choice(animals)}"


version_option = Option(
    None,
    "--version",
    help="Show the version of apx",
    callback=version_callback,
    is_eager=True,
)


# Helper functions for init command
def ensure_dir(path: Path) -> Path:
    """Create directory if it doesn't exist and return the path."""
    path.mkdir(parents=True, exist_ok=True)
    return path


def process_template_directory(
    source_dir: Path, target_dir: Path, app_name: str
) -> None:
    """
    Recursively process template directory, copying files and rendering Jinja2 templates.
    Replaces 'base' with app_name in paths.
    """
    for item in source_dir.rglob("*"):
        if item.is_file():
            # Calculate relative path from source_dir
            rel_path = item.relative_to(source_dir)

            # Replace 'base' with app_name in the path
            path_str = str(rel_path)
            if "/base/" in path_str or path_str.startswith("base/"):
                path_str = path_str.replace("/base/", f"/{app_name}/").replace(
                    "base/", f"{app_name}/"
                )

            # Determine target path
            if item.suffix == ".jinja2":
                # Remove .jinja2 extension for rendered files
                target_path = target_dir / path_str.removesuffix(".jinja2")
            else:
                target_path = target_dir / path_str

            # Ensure target directory exists
            target_path.parent.mkdir(parents=True, exist_ok=True)

            # Process file
            if item.suffix == ".jinja2":
                # Render Jinja2 template
                template = jinja2_env.get_template(f"base/{rel_path}")
                target_path.write_text(template.render(app_name=app_name))
                if item.name == "logo.svg.jinja2":
                    app_letter = app_name[0].upper()
                    target_path.write_text(template.render(app_letter=app_letter))
            else:
                # Copy file as-is
                shutil.copy(item, target_path)


def run_subprocess(cmd: list[str], cwd: Path, error_msg: str) -> None:
    """Run a subprocess and handle errors gracefully."""
    result = subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        console.print(f"[red]❌ {error_msg}[/red]")
        if result.stderr:
            console.print(f"[red]{result.stderr}[/red]")
        if result.stdout:
            console.print(f"[red]{result.stdout}[/red]")
        raise Exit(code=1)


@app.command(name="init", help="Initialize a new project")
def init(
    app_name: Annotated[
        str | None,
        Argument(
            help="The name of the project. Optional, will be generated if not provided"
        ),
    ] = None,
    app_path: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, the app will be created in the current working directory",
        ),
    ] = None,
    profile: Annotated[str | None, Option(help="The Databricks profile to use")] = None,
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
        process_template_directory(base_template_dir, app_path, app_name)

        # Create dist gitignore
        dist_dir = app_path / "src" / app_name / "__dist__"
        ensure_dir(dist_dir)
        (dist_dir / ".gitignore").write_text("*\n")

        # append DATABRICKS_PROFILE to .env if profile is provided
        if profile:
            with open(app_path / ".env", "a") as f:
                f.write(f"DATABRICKS_PROFILE={profile}\n")

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
        build_path=app_path / ".build",
        skip_ui_build=False,
        version=version,
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
    build_path: Annotated[
        Path,
        Option(help="Path to the build directory where artifacts will be placed"),
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
    cwd = Path.cwd()

    # === PHASE 1: Building UI ===
    if not skip_ui_build:
        with Progress(
            SpinnerColumn(),
            TextColumn("[progress.description]{task.description}"),
            console=console,
        ) as progress:
            task = progress.add_task(f"🎨 Building UI in {cwd.resolve()}...", total=None)

            result = subprocess.run(
                ["bun", "run", "build"],
                cwd=cwd,
                capture_output=True,
                text=True,
            )

            if result.returncode != 0:
                progress.update(task, description="❌ Failed to build UI", completed=True)
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
            ["uv", "build", "--wheel"],
            cwd=cwd,
            capture_output=True,
            text=True,
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

        build_dir = cwd / build_path

        # Clean up the build directory if it exists
        if build_dir.exists():
            shutil.rmtree(build_dir)

        # Find the built wheel file
        dist_dir = cwd / "dist"
        if not dist_dir.exists():
            console.print("[red]❌ dist/ directory not found[/red]")
            raise Exit(code=1)

        wheel_files = list(dist_dir.glob("*.whl"))
        if not wheel_files:
            console.print("[red]❌ No wheel file found in dist/[/red]")
            raise Exit(code=1)

        # Get the most recently created wheel file
        wheel_file = max(wheel_files, key=lambda p: p.stat().st_mtime)

        # Copy app.yml or app.yaml if it exists
        for app_file_name in ["app.yml", "app.yaml"]:
            app_file = cwd / app_file_name
            if app_file.exists():
                ensure_dir(build_dir)
                shutil.copy(app_file, build_dir / app_file_name)
                break

        # Copy the dist directory contents to build directory
        shutil.copytree(dist_dir, build_dir, dirs_exist_ok=True)

        # Write requirements.txt with the wheel file name
        reqs_file = build_dir / "requirements.txt"
        reqs_file.write_text(f"{wheel_file.name}\n")

        progress.update(task, description="✅ Build directory prepared", completed=True)

    console.print()
    console.print(f"[bold green]✨ Build completed successfully![/bold green]")
    console.print(
        f"[bold green]📦 Artifacts available in: {build_dir.resolve()}[/bold green]"
    )


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


def get_app_name_from_pyproject() -> str:
    """Read the app name from pyproject.toml."""
    pyproject_path = Path.cwd() / "pyproject.toml"
    if not pyproject_path.exists():
        console.print("[red]❌ pyproject.toml not found in current directory[/red]")
        raise Exit(code=1)

    with open(pyproject_path, "rb") as f:
        data = tomllib.load(f)

    # Get the project name from pyproject.toml
    app_name = data.get("project", {}).get("name")
    if not app_name:
        console.print("[red]❌ Could not find project name in pyproject.toml[/red]")
        raise Exit(code=1)

    return app_name


async def stream_output(proc, prefix: str, color: str):
    """Stream output from a subprocess with a colored prefix."""

    async def read_stream(stream, is_stderr=False):
        while True:
            line = await stream.readline()
            if not line:
                break
            text = line.decode().rstrip()
            if text:
                console.print(f"[{color}]{prefix}[/{color}] {text}")

    # Read stdout and stderr concurrently
    await asyncio.gather(
        read_stream(proc.stdout, is_stderr=False),
        read_stream(proc.stderr, is_stderr=True),
    )


async def run_frontend(frontend_port: int):
    """Run the frontend development server."""
    env = {**os.environ, "PORT": str(frontend_port)}

    proc = await asyncio.create_subprocess_exec(
        "bun",
        "run",
        "dev",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env=env,
        cwd=Path.cwd(),
    )

    await stream_output(proc, "[ui]", "cyan")
    await proc.wait()


@app.command(name="dev", help="Run development servers for frontend and backend")
def dev(
    frontend_port: Annotated[
        int, Option(help="Port for the frontend development server")
    ] = 5173,
    backend_port: Annotated[int, Option(help="Port for the backend server")] = 8000,
    backend_host: Annotated[
        str, Option(help="Host for the backend server")
    ] = "0.0.0.0",
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
                run_backend(Path.cwd(), app_module_name, backend_host, backend_port),
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
