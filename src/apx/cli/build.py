import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import Annotated

from typer import Argument, Exit, Option

from apx.cli.version import with_version
from apx.openapi import run_openapi
from apx.utils import (
    console,
    ensure_dir,
    format_elapsed_ms,
    generate_metadata_file,
    progress_spinner,
)


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
