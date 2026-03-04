from importlib.metadata import version

from apx._framework.app import App
from apx._framework import models

__version__ = version("apx")

__all__ = ["App", "models", "__version__"]


def _main() -> None:
    """CLI entrypoint — called via ``[project.scripts] apx = "apx:_main"``."""
    import sys

    from apx._core import run_cli

    sys.exit(run_cli(sys.argv))
