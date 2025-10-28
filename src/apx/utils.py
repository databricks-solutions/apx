import asyncio
from contextlib import contextmanager
import logging
import os
import random
import shutil
import subprocess
import time
import tomllib
from pathlib import Path
from typing import Any
from collections.abc import Generator
from typing_extensions import override

import jinja2
from rich.console import Console
from rich.markup import escape
from rich.progress import Progress, SpinnerColumn, TextColumn
from typer import Exit

console = Console()


def format_elapsed_ms(start_time_perf: float) -> str:
    """Format elapsed time since start_time_perf.

    If under 1 second, return milliseconds. Otherwise, return seconds and remaining milliseconds.
    """
    elapsed_seconds = time.perf_counter() - start_time_perf
    if elapsed_seconds < 1:
        return f"{int(elapsed_seconds * 1000)}ms"
    seconds = int(elapsed_seconds)
    remaining_ms = int((elapsed_seconds - seconds) * 1000)
    return f"{seconds}s {remaining_ms}ms"


@contextmanager
def progress_spinner(description: str, success_message: str):
    """Context manager for a transient progress spinner with completion message.

    Args:
        description: The description to show while the task is running
        success_message: The message to show after completion (without timing - will be added automatically)

    Yields:
        The start time (perf_counter) for the operation

    Example:
        with progress_spinner("📦 Installing dependencies...", "✅ Dependencies installed"):
            # do work
            pass
    """
    phase_start = time.perf_counter()

    with Progress(
        SpinnerColumn(finished_text=""),
        TextColumn("[progress.description]{task.description}"),
        console=console,
        transient=True,
    ) as progress:
        progress.add_task(description, total=None)
        yield phase_start

    console.print(f"{success_message} ({format_elapsed_ms(phase_start)})")


def print_with_prefix(prefix: str, text: str, color: str, width: int = 10):
    """Print text with a colored prefix.

    Args:
        prefix: The prefix text to display
        text: The main text to display
        color: The color for the prefix
        width: The width to pad the prefix to (default: 10)
    """
    # Get current timestamp with milliseconds
    current_time = time.time()
    timestamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(current_time))
    milliseconds = int((current_time % 1) * 1000)
    timestamp_with_ms = f"{timestamp}.{milliseconds:03d}"

    escaped_prefix = escape(prefix)
    # Pad the prefix to the specified width
    padded_prefix = escaped_prefix.ljust(width)

    # Handle multi-line text by adding prefix to each line
    lines = text.split("\n")
    for line in lines:
        escaped_line = escape(line)
        console.print(
            f"{timestamp_with_ms} | [{color}]{padded_prefix}[/] | {escaped_line}"
        )


class PrefixedLogHandler(logging.Handler):
    """A logging handler that uses print_with_prefix to output log messages."""

    def __init__(self, prefix: str, color: str, width: int = 10):
        super().__init__()
        self.prefix: str = prefix
        self.color: str = color
        self.width: int = width

    @override
    def emit(self, record: logging.LogRecord) -> None:
        try:
            msg = self.format(record)
            # Determine color based on log level
            color = self.color
            if record.levelno >= logging.ERROR:
                color = "red"
            elif record.levelno >= logging.WARNING:
                color = "yellow"

            print_with_prefix(self.prefix, msg, color, width=self.width)
        except Exception:
            self.handleError(record)


async def stream_output(
    proc: asyncio.subprocess.Process, prefix: str, color: str, width: int = 10
):
    """Stream output from a subprocess with a colored prefix."""

    async def read_stream(
        stream: asyncio.StreamReader, is_stderr: bool = False
    ) -> None:
        while True:
            line = await stream.readline()
            if not line:
                break
            _color = color if not is_stderr else "red"
            text = line.decode().rstrip()
            if text:
                print_with_prefix(prefix, text, _color, width=width)

    assert proc.stdout is not None and proc.stderr is not None, (
        "stdout and stderr must not be None"
    )
    # Read stdout and stderr concurrently
    await asyncio.gather(
        read_stream(proc.stdout, is_stderr=False),
        read_stream(proc.stderr, is_stderr=True),
    )


def is_uv_installed() -> bool:
    """Check if uv is installed on the system."""
    return shutil.which("uv") is not None


def is_bun_installed() -> bool:
    """Check if bun is installed on the system."""
    return shutil.which("bun") is not None


def random_name():
    """Generate a random docker-style name with dashes."""
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

    return f"{random.choice(adjectives)}-{random.choice(animals)}"


def ensure_dir(path: Path) -> None:
    """Create directory if it doesn't exist."""
    path.mkdir(parents=True, exist_ok=True)


def process_template_directory(
    source_dir: Path,
    target_dir: Path,
    app_name: str,
    app_slug: str,
    jinja2_env: jinja2.Environment,
) -> None:
    """
    Recursively process template directory, copying files and rendering Jinja2 templates.
    Replaces 'base' with app_slug in paths (for module names and directory structures).

    Args:
        source_dir: Source template directory
        target_dir: Target output directory
        app_name: User-facing app name (can contain dashes, e.g., 'my-app')
        app_slug: Internal app slug (with underscores, e.g., 'my_app') for module names and paths
        jinja2_env: Jinja2 environment for template rendering
    """
    # Get the templates root directory (parent of 'base' or 'addons')
    templates_root = jinja2_env.loader.searchpath[0]  # type: ignore

    # Calculate the relative path from templates root to source_dir
    source_rel_to_templates = source_dir.relative_to(templates_root)

    # Process both regular files and hidden files (starting with .)
    # Use set to avoid potential duplicates
    all_items = set(source_dir.rglob("*")) | set(source_dir.rglob(".*"))
    for item in all_items:
        if item.is_file():
            # Calculate relative path from source_dir
            rel_path = item.relative_to(source_dir)

            # Replace 'base' with app_slug in the path (for module names and paths)
            path_str = str(rel_path)
            if "/base/" in path_str or path_str.startswith("base/"):
                path_str = path_str.replace("/base/", f"/{app_slug}/").replace(
                    "base/", f"{app_slug}/"
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
                # Render Jinja2 template using the correct path relative to templates root
                template_path = str(source_rel_to_templates / rel_path)
                template: jinja2.Template = jinja2_env.get_template(template_path)
                # Pass both app_name (for display) and app_slug (for module names/paths) to templates
                target_path.write_text(
                    template.render(app_name=app_name, app_slug=app_slug)
                )
                if item.name == "logo.svg.jinja2":
                    app_letter = app_name[0].upper()
                    target_path.write_text(
                        template.render(
                            app_name=app_name, app_slug=app_slug, app_letter=app_letter
                        )
                    )
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


def get_project_metadata() -> dict[str, str]:
    """Read the project metadata from pyproject.toml."""
    pyproject_path = Path.cwd() / "pyproject.toml"
    if not pyproject_path.exists():
        console.print("[red]❌ pyproject.toml not found in current directory[/red]")
        raise Exit(code=1)
    with open(pyproject_path, "rb") as f:
        data = tomllib.load(f)
    return data["tool"]["apx"]["metadata"]


async def run_frontend(frontend_port: int):
    """Run the frontend development server."""
    proc = await asyncio.create_subprocess_exec(
        "bun",
        "run",
        "vite",
        "dev",
        stdout=asyncio.subprocess.PIPE,
        stderr=None,  # Let stderr pass through directly - don't capture it
        env=os.environ,
        cwd=Path.cwd(),
    )

    # Only capture stdout
    async def read_stdout():
        if proc.stdout is None:
            return
        while True:
            line = await proc.stdout.readline()
            if not line:
                break
            text = line.decode().rstrip()
            if text:
                print_with_prefix("[ui]", text, "cyan", width=10)

    await read_stdout()
    await proc.wait()


def generate_metadata_file(app_path: Path):
    pyproject_path = app_path / "pyproject.toml"
    pyproject: dict[str, Any] = tomllib.loads(pyproject_path.read_text())
    metadata: dict[str, str] = pyproject["tool"]["apx"]["metadata"]
    metadata_path = app_path / metadata["metadata-path"]

    metadata_path.write_text(
        "\n".join(
            [
                f'app_name = "{metadata["app-name"]}"',
                f'app_module = "{metadata["app-module"]}"',
                f'app_slug = "{metadata["app-slug"]}"',
            ]
        )
    )


def list_profiles() -> list[str]:
    import configparser

    cfg_path = os.path.expanduser("~/.databrickscfg")
    if not os.path.exists(cfg_path):
        return []
    parser = configparser.ConfigParser()
    parser.read(cfg_path)
    return list(parser.sections()) + ["DEFAULT"]


@contextmanager
def in_path(path: Path) -> Generator[None, None, None]:
    """Context manager to change the current working directory to the given path."""
    current_dir = os.getcwd()
    os.chdir(str(path))
    try:
        yield
    finally:
        os.chdir(current_dir)
