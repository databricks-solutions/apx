from importlib.metadata import version
import signal

__version__ = version("apx")

__all__ = ["__version__"]

GRACEFUL_EXIT_CODE = 128 + signal.SIGINT


def _main() -> None:
    """CLI entrypoint — called via ``[project.scripts] apx = "apx:_main"``."""
    import sys

    from apx._core import run_cli

    try:
        sys.exit(run_cli(sys.argv))
    except KeyboardInterrupt:
        sys.exit(GRACEFUL_EXIT_CODE)
