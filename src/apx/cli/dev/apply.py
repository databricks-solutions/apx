"""Apply addon command for the apx CLI."""

from importlib import resources
from pathlib import Path
from typing import Annotated

import jinja2
from rich.prompt import Confirm
from typer import Argument, Exit, Option

from apx.cli.version import with_version
from apx.models import Assistant, Layout, Template
from apx.utils import console, get_project_metadata, process_template_directory


def get_all_addon_names() -> list[str]:
    """Get all available addon names from Template, Assistant, and Layout enums.

    Returns a list of addon names that can be applied to a project.
    Only includes addons that are not the default/basic options.
    """
    addon_names: list[str] = []

    # Get addons from Template enum (exclude 'essential' as it's the base)
    for template in Template:
        if template != Template.essential:
            addon_names.append(template.value)

    # Get addons from Assistant enum (all assistants are addons)
    for assistant in Assistant:
        addon_names.append(assistant.value)

    # Get addons from Layout enum (exclude 'basic' as it's the base)
    for layout in Layout:
        if layout != Layout.basic:
            addon_names.append(layout.value)

    return addon_names


def get_addon_template_dir(addon_name: str) -> Path:
    """Get the template directory for an addon.

    Args:
        addon_name: The name of the addon

    Returns:
        Path to the addon's template directory

    Raises:
        Exit: If the addon is not found
    """
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")
    addon_dir = templates_dir / "addons" / addon_name

    if not addon_dir.exists():
        console.print(f"[red]❌ Addon '{addon_name}' not found in templates[/red]")
        raise Exit(code=1)

    return addon_dir


def check_file_conflicts(
    addon_dir: Path,
    target_dir: Path,
    app_slug: str,
) -> list[Path]:
    """Check which files from the addon would conflict with existing files.

    Args:
        addon_dir: The addon template directory
        target_dir: The target project directory
        app_slug: The app slug for path replacement

    Returns:
        List of file paths that would be overwritten
    """
    conflicting_files: list[Path] = []

    # Process both regular files and hidden files (starting with .)
    all_items = set(addon_dir.rglob("*")) | set(addon_dir.rglob(".*"))

    for item in all_items:
        if item.is_file():
            # Calculate relative path from addon_dir
            rel_path = item.relative_to(addon_dir)

            # Replace 'base' with app_slug in the path
            path_str = str(rel_path)
            if "/base/" in path_str or path_str.startswith("base/"):
                path_str = path_str.replace("/base/", f"/{app_slug}/").replace(
                    "base/", f"{app_slug}/"
                )

            # Determine target path (same logic as process_template_directory)
            if item.suffix == ".jinja2":
                target_path = target_dir / path_str.removesuffix(".jinja2")
            else:
                target_path = target_dir / path_str

            # Check if target file already exists
            if target_path.exists():
                conflicting_files.append(target_path)

    return conflicting_files


@with_version
def apply(
    addon_name: Annotated[
        str,
        Argument(
            help=f"The addon to apply. Available addons: {', '.join(get_all_addon_names())}"
        ),
    ],
    app_dir: Annotated[
        Path | None,
        Option(
            "--app-dir",
            help="The path to the app. If not provided, current working directory will be used",
        ),
    ] = None,
    force: Annotated[
        bool,
        Option(
            "--force",
            "-f",
            help="Apply addon without prompting for confirmation when files would be overwritten",
        ),
    ] = False,
):
    """Apply an addon to an existing project.

    Addons can add new features, integrations, or templates to your project.
    This command will check for conflicting files and prompt for confirmation
    before overwriting them (unless --force is used).
    """
    # Set default app_dir
    if app_dir is None:
        app_dir = Path.cwd()

    # Validate addon name
    available_addons = get_all_addon_names()
    if addon_name not in available_addons:
        console.print(f"[red]❌ Invalid addon: {addon_name}[/red]")
        console.print(
            f"[yellow]Available addons: {', '.join(available_addons)}[/yellow]"
        )
        raise Exit(code=1)

    # Get project metadata
    try:
        metadata = get_project_metadata()
        app_name = metadata.app_name
        app_slug = metadata.app_slug
    except Exception as e:
        console.print(f"[red]❌ Failed to read project metadata: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you're in a valid apx project directory[/yellow]"
        )
        raise Exit(code=1)

    # Get addon template directory
    addon_dir = get_addon_template_dir(addon_name)

    console.print(f"[cyan]📦 Applying addon '{addon_name}' to project...[/cyan]")

    # Check for conflicting files
    conflicting_files = check_file_conflicts(addon_dir, app_dir, app_slug)

    if conflicting_files:
        console.print(
            f"[yellow]⚠️  The following {len(conflicting_files)} file(s) will be overwritten:[/yellow]"
        )
        console.print()
        for file_path in conflicting_files:
            # Show relative path from app_dir
            rel_path = file_path.relative_to(app_dir)
            console.print(f"  - {rel_path}")
        console.print()

        if not force:
            # Prompt user for confirmation
            if not Confirm.ask(
                "[yellow]Do you want to continue and overwrite these files?[/yellow]",
                default=False,
            ):
                console.print("[yellow]❌ Addon application cancelled[/yellow]")
                raise Exit(code=0)

    # Setup Jinja2 environment
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")
    jinja2_env: jinja2.Environment = jinja2.Environment(
        loader=jinja2.FileSystemLoader(templates_dir)
    )

    # Apply the addon
    process_template_directory(addon_dir, app_dir, app_name, app_slug, jinja2_env)

    console.print(
        f"[bold green]✨ Addon '{addon_name}' applied successfully![/bold green]"
    )

    # Provide helpful next steps based on the addon
    if addon_name in ["cursor", "vscode", "codex", "claude"]:
        console.print(
            "[cyan]💡 Assistant rules have been configured. Restart your editor to apply changes.[/cyan]"
        )
    elif addon_name == "sidebar":
        console.print(
            "[cyan]💡 Sidebar layout has been added. You may need to install additional dependencies:[/cyan]"
        )
        console.print(
            "   [dim]bun add avatar sidebar separator skeleton badge card[/dim]"
        )
    elif addon_name == "stateful":
        console.print(
            "[cyan]💡 Stateful template has been applied. Your backend now supports state management.[/cyan]"
        )
