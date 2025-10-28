from pathlib import Path
from typing import Annotated

from typer import Argument, Option

from apx.cli.version import with_version
from apx.openapi import run_openapi


@with_version
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
    force: Annotated[
        bool,
        Option(
            "--force", "-f", help="Force regeneration even if schema hasn't changed"
        ),
    ] = False,
) -> None:
    """Generate OpenAPI schema from FastAPI app and run orval to generate client."""
    if app_dir is None:
        app_dir = Path.cwd()

    run_openapi(app_dir, watch=watch, force=force)

