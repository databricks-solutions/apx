import sys

from apx._core import run_cli


def main():
    try:
        raise SystemExit(run_cli(sys.argv))
    except KeyboardInterrupt:
        # Gracefully handle Ctrl+C (e.g., from `apx dev logs -f`)
        raise SystemExit(0)


if __name__ == "__main__":
    main()
