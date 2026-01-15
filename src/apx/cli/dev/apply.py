"""Apply addon command for the apx CLI."""

from importlib import resources
from pathlib import Path
from typing import Annotated

import jinja2
from rich.prompt import Confirm
from typer import Argument, Exit, Option

from apx.cli.version import with_version
from apx.models import Assistant, Layout, ProjectMetadata, Template
from apx.utils import console, process_template_directory


def get_all_addon_names() -> list[str]:
    """Get all available addon names from Template, Assistant, and Layout enums.

    Returns a list of addon names that can be applied to a project.
    Includes all templates (including 'essential'), assistants, and non-basic layouts.
    """
    addon_names: list[str] = []

    # Get addons from Template enum (now includes 'essential' for full template reapplication)
    for template in Template:
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
        addon_name: The name of the addon (or 'essential'/'base' for the base template)

    Returns:
        Path to the addon's template directory

    Raises:
        Exit: If the addon is not found
    """
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")

    # 'essential' and 'base' map to the base template directory
    if addon_name in ("essential", "base"):
        addon_dir = templates_dir / "base"
    else:
        addon_dir = templates_dir / "addons" / addon_name

    if not addon_dir.exists():
        console.print(f"[red]❌ Addon '{addon_name}' not found in templates[/red]")
        raise Exit(code=1)

    return addon_dir


def get_all_template_sources() -> list[str]:
    """Get all template source names including 'base', 'essential', and addons.

    Returns a list of all template sources that can be used with --file option.
    Includes the base template (and its 'essential' alias) and all addon templates.
    """
    return ["base", "essential"] + get_all_addon_names()


def get_template_source_dir(source_name: str) -> Path:
    """Get the template directory for a source (base, essential, or addon).

    Args:
        source_name: The name of the template source ('base', 'essential', or an addon name)

    Returns:
        Path to the template source directory

    Raises:
        Exit: If the template source is not found
    """
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")

    # 'essential' is an alias for 'base' template
    if source_name in ("base", "essential"):
        source_dir = templates_dir / "base"
    else:
        source_dir = templates_dir / "addons" / source_name

    if not source_dir.exists():
        console.print(
            f"[red]❌ Template source '{source_name}' not found in templates[/red]"
        )
        raise Exit(code=1)

    return source_dir


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


def calculate_target_path(
    source_file: Path,
    target_dir: Path,
    app_slug: str,
    source_dir: Path,
) -> Path:
    """Calculate the target path for a template file without processing it.

    Args:
        source_file: The source file path in the template directory
        target_dir: The target project directory
        app_slug: The internal app slug for module names
        source_dir: The root directory of the template source

    Returns:
        The target path where the file would be written
    """
    # Calculate relative path from source_dir
    rel_path = source_file.relative_to(source_dir)

    # Replace 'base' with app_slug in the path
    path_str = str(rel_path)
    if "/base/" in path_str or path_str.startswith("base/"):
        path_str = path_str.replace("/base/", f"/{app_slug}/").replace(
            "base/", f"{app_slug}/"
        )

    # Determine target path
    if source_file.suffix == ".jinja2":
        target_path = target_dir / path_str.removesuffix(".jinja2")
    else:
        target_path = target_dir / path_str

    return target_path


def process_single_template_file(
    source_file: Path,
    target_dir: Path,
    app_name: str,
    app_slug: str,
    jinja2_env: jinja2.Environment,
    source_dir: Path,
) -> Path:
    """Process a single template file and return the target path.

    Args:
        source_file: The source file path in the template directory
        target_dir: The target project directory
        app_name: The user-facing app name
        app_slug: The internal app slug for module names
        jinja2_env: Jinja2 environment for template rendering
        source_dir: The root directory of the template source

    Returns:
        The target path where the file was written

    Raises:
        Exit: If the source file doesn't exist
    """
    import shutil

    if not source_file.exists():
        console.print(f"[red]❌ File not found: {source_file}[/red]")
        raise Exit(code=1)

    if not source_file.is_file():
        console.print(f"[red]❌ Path is not a file: {source_file}[/red]")
        raise Exit(code=1)

    # Calculate target path
    target_path = calculate_target_path(source_file, target_dir, app_slug, source_dir)

    # Ensure target directory exists
    target_path.parent.mkdir(parents=True, exist_ok=True)

    # Process file
    if source_file.suffix == ".jinja2":
        # Get templates root directory
        assert isinstance(jinja2_env.loader, jinja2.FileSystemLoader), (
            "Loader must be a FileSystemLoader"
        )
        templates_root = jinja2_env.loader.searchpath[0]

        # Calculate the relative path from templates root to source_file
        source_rel_to_templates = source_file.relative_to(templates_root)
        template_path = source_rel_to_templates.as_posix()

        # Render Jinja2 template
        template: jinja2.Template = jinja2_env.get_template(template_path)

        # Special handling for logo.svg.jinja2
        if source_file.name == "logo.svg.jinja2":
            app_letter = app_name[0].upper()
            target_path.write_text(
                template.render(  # pyright:ignore[reportUnknownMemberType]
                    app_name=app_name, app_slug=app_slug, app_letter=app_letter
                ),
                encoding="utf-8",
            )
        else:
            target_path.write_text(
                template.render(  # pyright:ignore[reportUnknownMemberType]
                    app_name=app_name, app_slug=app_slug
                ),
                encoding="utf-8",
            )
    else:
        # Copy file as-is
        shutil.copy(source_file, target_path)

    return target_path


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
    file_path: Annotated[
        str | None,
        Option(
            "--file",
            help="Apply a single file from the template (path relative to template root)",
        ),
    ] = None,
):
    """Apply an addon to an existing project.

    Addons can add new features, integrations, or templates to your project.
    This command will check for conflicting files and prompt for confirmation
    before overwriting them (unless --force is used).

    When --file is specified, applies a single file from the template source
    (including 'base'). Otherwise, applies the entire addon.
    """
    # Set default app_dir
    if app_dir is None:
        app_dir = Path.cwd()

    # Validate addon/template source name
    if file_path is not None:
        # When --file is specified, allow 'base', 'essential', and all addons
        available_sources = get_all_template_sources()
        if addon_name not in available_sources:
            console.print(f"[red]❌ Invalid template source: {addon_name}[/red]")
            console.print(
                f"[yellow]Available sources: {', '.join(available_sources)}[/yellow]"
            )
            raise Exit(code=1)
    else:
        # When --file is not specified, only allow addons (not 'base' or 'essential')
        available_addons = get_all_addon_names()
        if addon_name not in available_addons:
            console.print(f"[red]❌ Invalid addon: {addon_name}[/red]")
            console.print(
                f"[yellow]Available addons: {', '.join(available_addons)}[/yellow]"
            )
            raise Exit(code=1)

    # Get project metadata
    try:
        metadata = ProjectMetadata.read()
        app_name = metadata.app_name
        app_slug = metadata.app_slug
    except Exception as e:
        console.print(f"[red]❌ Failed to read project metadata: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you're in a valid apx project directory[/yellow]"
        )
        raise Exit(code=1)

    # Setup Jinja2 environment
    templates_dir: Path = Path(str(resources.files("apx"))).joinpath("templates")
    jinja2_env: jinja2.Environment = jinja2.Environment(
        loader=jinja2.FileSystemLoader(templates_dir)
    )

    # Handle single file application
    if file_path is not None:
        # Get template source directory
        source_dir = get_template_source_dir(addon_name)

        # Construct full source file path
        source_file = source_dir / file_path

        console.print(
            f"[cyan]📦 Applying file '{file_path}' from '{addon_name}' template...[/cyan]"
        )

        # Check if the source file exists
        if not source_file.exists():
            console.print(
                f"[red]❌ File '{file_path}' not found in '{addon_name}' template[/red]"
            )
            raise Exit(code=1)

        if not source_file.is_file():
            console.print(
                f"[red]❌ Path '{file_path}' is not a file in '{addon_name}' template[/red]"
            )
            raise Exit(code=1)

        # Calculate target path (without processing yet)
        target_path = calculate_target_path(source_file, app_dir, app_slug, source_dir)
        rel_target_path = target_path.relative_to(app_dir)

        # Check if file would be overwritten and prompt if needed
        if target_path.exists() and not force:
            console.print(
                f"[yellow]⚠️  File will be overwritten: {rel_target_path}[/yellow]"
            )
            if not Confirm.ask(
                "[yellow]Do you want to continue and overwrite this file?[/yellow]",
                default=False,
            ):
                console.print("[yellow]❌ File application cancelled[/yellow]")
                raise Exit(code=0)

        # Process the single file after confirmation (or with --force)
        target_path = process_single_template_file(
            source_file, app_dir, app_name, app_slug, jinja2_env, source_dir
        )

        console.print(
            f"[bold green]✨ File '{file_path}' applied successfully to {rel_target_path}![/bold green]"
        )
        return

    # Handle full addon application (existing behavior)
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
        for conflict_file_path in conflicting_files:
            # Show relative path from app_dir
            rel_path = conflict_file_path.relative_to(app_dir)
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
