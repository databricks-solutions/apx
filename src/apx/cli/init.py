import os
import shutil
import subprocess
import time
from importlib import resources
from pathlib import Path
from typing import Annotated

import jinja2
from dotenv import set_key
from rich import print
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.prompt import Confirm, Prompt
from typer import Argument, Exit, Option

from apx.cli.version import with_version
from apx.models import Assistant, Layout, ProjectMetadata, Template
from apx.utils import (
    console,
    ensure_dir,
    format_elapsed_ms,
    is_bun_installed,
    is_uv_installed,
    list_profiles,
    process_template_directory,
    progress_spinner,
    random_name,
    run_subprocess,
)


def is_in_git_repo(path: Path) -> bool:
    """
    Check if a path is inside a git repository (either itself or a parent directory).

    Args:
        path: The path to check

    Returns:
        True if the path is inside a git work tree, False otherwise
    """
    result = subprocess.run(
        ["git", "rev-parse", "--is-inside-work-tree"],
        cwd=path,
        capture_output=True,
        text=True,
    )
    return result.returncode == 0 and result.stdout.strip() == "true"


def bun_install(cwd: Path) -> None:
    """
    Run bun install command with optional cache directory support.

    Args:
        cwd: Current working directory for the command
    """
    cmd = ["bun", "install"]

    # Check if BUN_CACHE_DIR is set and add cache directory flag
    bun_cache_dir = os.environ.get("BUN_CACHE_DIR")
    if bun_cache_dir:
        cache_path = Path(bun_cache_dir).resolve()
        cmd.extend(["--cache-dir", str(cache_path)])

    run_subprocess(cmd, cwd=cwd, error_msg="Failed to install dependencies")


def add_shadcn_components(
    cwd: Path,
    args: list[str],
) -> None:
    base_cmd = ["bun", "x", "--bun"]
    base_cmd.extend(["shadcn@latest", "add", *args, "--yes", "--overwrite"])
    run_subprocess(base_cmd, cwd=cwd, error_msg="Failed to add shadcn components")


@with_version
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
        Template | None,
        Option(
            "--template",
            "-t",
            help="The template to use. Will prompt if not provided",
        ),
    ] = None,
    profile: Annotated[
        str | None,
        Option(
            "--profile",
            "-p",
            help="The Databricks profile to use. Will prompt if not provided",
        ),
    ] = None,
    assistant: Annotated[
        Assistant | None,
        Option(
            "--assistant",
            "-a",
            help="The type of assistant to use (cursor/vscode/codex/claude). Will prompt if not provided",
        ),
    ] = None,
    layout: Annotated[
        Layout | None,
        Option(
            "--layout",
            "-l",
            help="The layout to use. Will prompt if not provided",
        ),
    ] = None,
    apx_package: Annotated[
        str | None,
        Option(
            "--apx-package",
            "-apx",
            hidden=True,
            help="The apx package to install. Used for internal testing and development.",
        ),
    ] = "https://github.com/databricks-solutions/apx.git",
    apx_editable: Annotated[
        bool,
        Option(
            "--apx-editable",
            "-apx-e",
            hidden=True,
            help="Whether to install apx as editable package.",
        ),
    ] = False,
    skip_frontend_dependencies: Annotated[
        bool,
        Option(
            "--skip-frontend-dependencies",
            help="Skip installing frontend dependencies (bun packages).",
        ),
    ] = False,
    skip_backend_dependencies: Annotated[
        bool,
        Option(
            "--skip-backend-dependencies",
            help="Skip installing backend dependencies (uv sync).",
        ),
    ] = False,
    skip_build: Annotated[
        bool,
        Option(
            "--skip-build",
            help="Skip building the project after initialization.",
        ),
    ] = False,
):
    """Initialize a new project."""
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

    # Setup templates
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")
    jinja2_env: jinja2.Environment = jinja2.Environment(
        loader=jinja2.FileSystemLoader(templates_dir)
    )

    console.print("[bold chartreuse1]Welcome to apx 🚀[/bold chartreuse1]\n")

    # Prompt for app name if not provided
    if app_name is None:
        default_name = random_name()
        app_name = Prompt.ask(
            "[cyan]What's the name of your app?[/cyan]",
            default=default_name,
        )

    # Normalize app name: convert to lowercase and replace spaces with dashes
    assert isinstance(app_name, str), "app_name must be a string"  # make mypy happy

    app_name = app_name.lower().replace(" ", "-").replace("_", "-")
    # Validate that app_name only contains alphanumeric characters and dashes
    if not app_name.replace("-", "").isalnum():
        print(
            "[red]Invalid app name. Please use only alphanumeric characters and dashes.[/red]"
        )
        return Exit(code=1)

    # Create app_slug: internal version with underscores for module names and paths
    app_slug = app_name.replace("-", "_")

    # Prompt for template if not provided
    if template is None:
        prompt_template = Prompt.ask(
            "[cyan]Which template would you like to use?[/cyan]",
            choices=[template.value for template in Template],
            default=Template.essential.value,
        )

        template = Template.from_string(prompt_template)

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
            prompt_assistant = Prompt.ask(
                "[cyan]Which assistant would you like to use?[/cyan]",
                choices=[assistant.value for assistant in Assistant],
                default=Assistant.cursor.value,
            )
            assistant = Assistant.from_string(prompt_assistant)

    # Prompt for layout if not provided
    if layout is None:
        prompt_layout = Prompt.ask(
            "[cyan]Which layout would you like to use?[/cyan]",
            choices=[layout.value for layout in Layout],
            default=Layout.sidebar.value,
        )
        layout = Layout.from_string(prompt_layout)

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

        if template == Template.stateful:
            # replace databricks.yml.jinja2 with databricks.yml.jinja2 from addons/stateful
            stateful_addon = templates_dir / "addons/stateful"
            process_template_directory(
                stateful_addon, app_path, app_name, app_slug, jinja2_env
            )

        # append DATABRICKS_CONFIG_PROFILE to .env if profile is provided
        if profile:
            set_key(app_path / ".env", "DATABRICKS_CONFIG_PROFILE", profile)

        if layout == Layout.sidebar:
            # replace src/base/ui/routes/__root.tsx with src/base/ui/routes/__root.tsx from addons/sidebar
            sidebar_addon = templates_dir / "addons/sidebar"
            process_template_directory(
                sidebar_addon, app_path, app_name, app_slug, jinja2_env
            )

    # === PHASE 2: Installing frontend dependencies ===
    if not skip_frontend_dependencies:
        with progress_spinner(
            "📦 Installing frontend dependencies...",
            "✅ Frontend dependencies installed",
        ):
            # Install all dependencies from package.json
            bun_install(app_path)

    # === PHASE 3: Bootstrapping shadcn ===
    if not skip_frontend_dependencies:
        with progress_spinner(
            "🎨 Bootstrapping shadcn components...", "✅ Shadcn components added"
        ):
            # Add button component
            add_shadcn_components(app_path, ["button"])

            if layout == Layout.sidebar:
                # install necessary components for sidebar layout
                add_shadcn_components(
                    app_path,
                    [
                        "avatar",
                        "sidebar",
                        "separator",
                        "skeleton",
                        "badge",
                        "sidebar",
                        "card",
                    ],
                )

    # === PHASE 4: Initializing git ===
    if is_in_git_repo(app_path):
        console.print("[dim]⏭️  Skipping git init (already in a git repository)[/dim]")
    else:
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
    if not skip_backend_dependencies:
        phase_start = time.perf_counter()
        with Progress(
            SpinnerColumn(finished_text=""),
            TextColumn("[progress.description]{task.description}"),
            console=console,
            transient=True,
        ) as progress:
            task = progress.add_task("🐍 Setting up project...", total=None)

            # Generate the _metadata.py file
            metadata = ProjectMetadata.read(app_path)
            metadata.generate_metadata_file(app_path)
            # add apx package:
            if apx_package:
                base_cmd = ["uv", "add", "--dev"]
                if apx_editable:
                    base_cmd.append("--editable")
                final_cmd = base_cmd + [apx_package]
                result = subprocess.run(
                    final_cmd,
                    cwd=app_path,
                    capture_output=True,
                    text=True,
                    env=os.environ,
                )

                if result.returncode != 0:
                    console.print("[red]❌ Failed to add apx package[/red]")
                    if result.stderr:
                        console.print(f"[red]{result.stderr}[/red]")
                    if result.stdout:
                        console.print(f"[red]{result.stdout}[/red]")
                    raise Exit(code=1)

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

    # we're using the uv command because it needs to run the build command
    # in the virtual environment of the project, not the global one

    if not skip_build:
        with progress_spinner("🔧 Building project...", "✅ Project built"):
            result = subprocess.run(
                ["uv", "run", "apx", "build"],
                cwd=app_path,
                capture_output=True,
                text=True,
                env=os.environ,
            )

            if result.returncode != 0:
                console.print("[red]❌ Failed to build project[/red]")
                if result.stderr:
                    console.print(f"[red]{result.stderr}[/red]")
                if result.stdout:
                    console.print(f"[red]{result.stdout}[/red]")
                raise Exit(code=1)

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

            if assistant == Assistant.vscode:
                progress.update(task, description="🤖 Copying VSCode instructions...")
                rules_addon = templates_dir / "addons/vscode"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )
            elif assistant == Assistant.cursor:
                progress.update(task, description="🤖 Copying Cursor rules...")
                rules_addon = templates_dir / "addons/cursor"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )
            elif assistant == Assistant.claude:
                progress.update(task, description="🤖 Copying Claude rules...")
                rules_addon = templates_dir / "addons/claude"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )
            elif assistant == Assistant.codex:
                progress.update(task, description="🤖 Copying Codex rules...")
                rules_addon = templates_dir / "addons/codex"
                process_template_directory(
                    rules_addon, app_path, app_name, app_slug, jinja2_env
                )

                console.print(
                    "[yellow]Please note that Codex mcp config is not supported yet.[/]"
                )
                console.print(
                    "[yellow]Follow this guide to set it up manually: https://ui.shadcn.com/docs/mcp#codex [/]"
                )

        console.print(
            f"✅ Assistant rules configured ({format_elapsed_ms(phase_start)})"
        )

    console.print()
    console.print(
        f"[bold green]✨ Project {app_name} initialized successfully! [/bold green]"
    )
    console.print(
        f"[bold green]🚀 Run `cd {app_path.resolve()} && uv run apx dev start` to get started![/bold green]"
    )
