import sys

from apx._core import run_cli

__version__ = "0.1.0"

def main() -> None:
    raise SystemExit(run_cli(sys.argv))