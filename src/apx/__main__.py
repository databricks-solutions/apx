import importlib
import json
import shutil
import subprocess
import time
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
        "snake",
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


def render_jinja_template(template_name: str, target_path: Path, **context):
    """Render a Jinja2 template and write to target path."""
    template = jinja2_env.get_template(template_name)
    target_path.write_text(template.render(**context))


def copy_template(template_name: str, target_path: Path):
    """Copy a template file to target path."""
    source = templates_dir.joinpath(template_name)
    shutil.copy(source, target_path)


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
    app_name: Annotated[str | None, Argument(help="The name of the project")] = None,
    app_path: Annotated[
        Path | None,
        Argument(
            help="The path to the app. If not provided, the app will be created in the current working directory",
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

        # Create directory structure
        ensure_dir(app_path)
        src_dir = ensure_dir(app_path / "src" / app_name)
        api_dir = ensure_dir(src_dir / "api")
        ui_dir = ensure_dir(src_dir / "ui")
        ensure_dir(ui_dir / "lib")
        ensure_dir(ui_dir / "styles")
        ensure_dir(ui_dir / "routes")
        ensure_dir(ui_dir / "components")
        ensure_dir(ui_dir / "types")
        ensure_dir(app_path / ".cursor" / "rules")
        dist_dir = ensure_dir(src_dir / "__dist__")

        # Render Jinja templates
        render_jinja_template(
            "base/README.md.jinja2", app_path / "README.md", app_name=app_name
        )
        render_jinja_template(
            "base/.gitignore.jinja2", app_path / ".gitignore", app_name=app_name
        )
        render_jinja_template(
            "base/pyproject.toml.jinja2", app_path / "pyproject.toml", app_name=app_name
        )
        render_jinja_template(
            "base/package.json.jinja2", app_path / "package.json", app_name=app_name
        )
        render_jinja_template(
            "base/components.json.jinja2",
            app_path / "components.json",
            app_name=app_name,
        )
        render_jinja_template(
            "base/tsconfig.json.jinja2", app_path / "tsconfig.json", app_name=app_name
        )
        render_jinja_template(
            "base/vite.config.ts.jinja2", app_path / "vite.config.ts", app_name=app_name
        )
        render_jinja_template(
            "base/src/base/ui/index.html.jinja2",
            ui_dir / "index.html",
            app_name=app_name,
        )
        render_jinja_template(
            "base/.cursor/rules/project.mdc.jinja2",
            app_path / ".cursor" / "rules" / "project.mdc",
            app_name=app_name,
        )
        render_jinja_template(
            "base/src/base/api/app.py.jinja2", api_dir / "app.py", app_name=app_name
        )

        # Copy static files
        copy_template("base/src/base/__init__.py", src_dir / "__init__.py")
        copy_template("base/src/base/_version.pyi", src_dir / "_version.pyi")
        copy_template("base/src/base/ui/lib/utils.ts", ui_dir / "lib" / "utils.ts")
        copy_template(
            "base/src/base/ui/styles/globals.css", ui_dir / "styles" / "globals.css"
        )
        copy_template("base/src/base/ui/main.tsx", ui_dir / "main.tsx")
        render_jinja_template(
            "base/src/base/ui/routes/index.tsx.jinja2",
            ui_dir / "routes" / "index.tsx",
            app_name=app_name,
        )
        copy_template(
            "base/src/base/ui/routes/__root.tsx", ui_dir / "routes" / "__root.tsx"
        )
        copy_template(
            "base/src/base/ui/components/mode-toggle.tsx",
            ui_dir / "components" / "mode-toggle.tsx",
        )
        copy_template(
            "base/src/base/ui/components/theme-provider.tsx",
            ui_dir / "components" / "theme-provider.tsx",
        )
        copy_template(
            "base/src/base/ui/types/vite-env.d.ts", ui_dir / "types" / "vite-env.d.ts"
        )
        copy_template(
            "base/src/base/ui/lib/selector.ts", ui_dir / "lib" / "selector.ts"
        )

        # Create dist gitignore
        (dist_dir / ".gitignore").write_text("*\n")

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

    console.print()
    console.print(
        f"[bold green]✨ Project {app_name} initialized successfully! [/bold green]"
    )
    console.print(
        f"[bold green]🚀 Run `cd {app_path.resolve()}` to get started![/bold green]"
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


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
