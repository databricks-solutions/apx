import os
from datetime import datetime, timezone
from pathlib import Path
import shutil
import subprocess
import time
from typing import Annotated

from typer import Argument, Exit, Option

from apx.cli.version import with_version
from apx.cli.openapi import run_openapi
from apx.models import ProjectMetadata
from apx.utils import (
    console,
    ensure_apx_plugin,
    ensure_dir,
    format_elapsed_ms,
    progress_spinner,
)

DEFAULT_FALLBACK_VERSION = "0.0.0"


def get_base_version(app_path: Path) -> str:
    """
    Get the base version using hatch version command.
    Falls back to DEFAULT_FALLBACK_VERSION if the command fails.
    """
    try:
        result = subprocess.run(
            ["uv", "run", "hatch", "version"],
            cwd=app_path,
            capture_output=True,
            text=True,
            env=os.environ,
        )
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout.strip()
    except Exception:
        pass
    return DEFAULT_FALLBACK_VERSION


def generate_build_version(base_version: str) -> str:
    """
    Generate a build version with timestamp suffix.

    If base_version already has a local version identifier (contains '+'),
    the timestamp is appended with '.' separator to comply with PEP 440.
    Otherwise, '+' is used as the separator.

    Examples:
        - "1.2.3" -> "1.2.3+20260114120000"
        - "0.0.0.post1.dev0+abc123" -> "0.0.0.post1.dev0+abc123.20260114120000"
    """
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d%H%M%S")
    if "+" in base_version:
        # Already has a local version identifier, append with '.'
        return f"{base_version}.{timestamp}"
    return f"{base_version}+{timestamp}"


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
) -> None:
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

    # ensure .apx/plugin.ts exists
    ensure_apx_plugin(app_path)

    start_time_perf = time.perf_counter()
    # Clean up the build directory if it exists
    if build_dir.exists():
        shutil.rmtree(build_dir)

    # ensure the build directory exists
    ensure_dir(build_dir)

    # add a .build/.gitignore file
    (build_dir / ".gitignore").write_text("*\n")

    # Read project metadata and generate the _metadata.py file
    metadata = ProjectMetadata.read(app_path)
    metadata.generate_metadata_file(app_path)

    # Ensure __dist__ directory exists before loading app
    # (needed because app imports StaticFiles pointing to __dist__)
    dist_dir = app_path / "src" / metadata.app_slug / "__dist__"
    ensure_dir(dist_dir)
    (dist_dir / ".gitignore").write_text("*\n")

    # Generate the openapi schema and API client
    run_openapi(app_path)

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
    # Get base version and generate build version with timestamp
    base_version = get_base_version(app_path)
    build_version = generate_build_version(base_version)

    # Prepare environment with UV_DYNAMIC_VERSIONING_BYPASS
    build_env = os.environ.copy()
    build_env["UV_DYNAMIC_VERSIONING_BYPASS"] = build_version

    with progress_spinner("🐍 Building Python wheel...", "✅ Python wheel built"):
        result = subprocess.run(
            ["uv", "build", "--wheel", "--out-dir", str(build_path)],
            cwd=app_path,
            capture_output=True,
            text=True,
            env=build_env,
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

    wheel_files = list(build_dir.glob("*.whl"))
    if not wheel_files:
        console.print("[red]❌ No wheel file found in build directory[/red]")
        raise Exit(code=1)

    wheel_file = wheel_files[0]
    wheel_file_name = wheel_file.name

    # write requirements.txt with the wheel file name
    requirements_file = build_dir / "requirements.txt"
    requirements_file.write_text(f"{wheel_file_name}\n")

    console.print(f"✅ Full build completed in ({format_elapsed_ms(start_time_perf)})")
