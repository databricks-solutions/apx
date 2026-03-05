from importlib.metadata import version

__version__ = version("apx")

__all__ = ["__version__"]


def _main() -> None:
    """CLI entrypoint — called via ``[project.scripts] apx = "apx:_main"``."""
    import sys

    from apx._core import run_cli

    sys.exit(run_cli(sys.argv))
