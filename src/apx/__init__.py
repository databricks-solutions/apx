import sys

from apx._core import get_bun_binary_path, run_cli

__version__ = "0.1.0"


def main() -> None:
    raise SystemExit(run_cli(sys.argv))


__all__ = ["__version__", "get_bun_binary_path", "main"]
