# /// script
# requires-python = ">=3.11"
# dependencies = ["tomlkit"]
# ///

"""Generate a test project using a local apx build.

Usage:
    uv run --script scripts/dev/gen.py <folder> <profile> [extra-args...]

Steps:
    1. maturin build -j 6 -o dist
    2. rm -rf <folder>
    3. uvx --from <wheel> apx init <folder> -p <profile> [extra-args...]
    4. Patch pyproject.toml to use the local wheel from dist/
    5. Patch MCP config files to use `uv run apx` instead of bare `apx`
    6. uv sync
    7. uv run apx dev check
"""

import json
import os
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

import tomlkit

BOLD = "\033[1m"
DIM = "\033[2m"
GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
CYAN = "\033[36m"
RESET = "\033[0m"


def fmt_duration(seconds: float) -> str:
    if seconds < 1:
        return f"{seconds * 1000:.0f}ms"
    if seconds < 60:
        return f"{seconds:.1f}s"
    m, s = divmod(seconds, 60)
    return f"{int(m)}m {s:.1f}s"


@contextmanager
def stage(name: str, step: int, total: int):
    prefix = f"{DIM}[{step}/{total}]{RESET}"
    print(f"\n{prefix} {CYAN}{BOLD}{name}{RESET}")
    t0 = time.monotonic()
    try:
        yield
    except Exception:
        elapsed = time.monotonic() - t0
        print(
            f"{prefix} {RED}{BOLD}FAILED{RESET} {DIM}({fmt_duration(elapsed)}){RESET}"
        )
        raise
    else:
        elapsed = time.monotonic() - t0
        print(f"{prefix} {GREEN}done{RESET} {DIM}({fmt_duration(elapsed)}){RESET}")


def run(cmd: list[str], **kwargs) -> None:
    print(f"  {DIM}$ {' '.join(cmd)}{RESET}")
    result = subprocess.run(cmd, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(
            f"Command failed with exit code {result.returncode}: {' '.join(cmd)}"
        )


def find_wheel(dist_dir: Path) -> Path:
    wheels = sorted(
        dist_dir.glob("*.whl"), key=lambda p: os.path.getmtime(p), reverse=True
    )
    if not wheels:
        raise FileNotFoundError(f"No wheel found in {dist_dir}")
    return wheels[0]


def patch_pyproject(pyproject_path: Path, wheel_path: Path) -> None:
    """Remove the apx-index source config and point apx dep to local wheel."""
    doc = tomlkit.parse(pyproject_path.read_text())

    # Remove [[tool.uv.index]] entry for apx-index
    tool_uv = doc.get("tool", {}).get("uv", {})
    if "index" in tool_uv:
        indexes = tool_uv["index"]
        tool_uv["index"] = [idx for idx in indexes if idx.get("name") != "apx-index"]
        if not tool_uv["index"]:
            del tool_uv["index"]

    # Remove [tool.uv.sources].apx
    sources = tool_uv.get("sources", {})
    if "apx" in sources:
        del sources["apx"]
    if not sources and "sources" in tool_uv:
        del tool_uv["sources"]

    # Replace apx dependency in [dependency-groups].dev with local wheel path
    dev_deps = doc.get("dependency-groups", {}).get("dev", [])
    new_deps = []
    for dep in dev_deps:
        if isinstance(dep, str) and dep.startswith("apx"):
            new_deps.append(f"apx @ {wheel_path.as_uri()}")
        else:
            new_deps.append(dep)

    dep_groups = doc.get("dependency-groups", {})
    dep_groups["dev"] = new_deps

    pyproject_path.write_text(tomlkit.dumps(doc))
    print(f"  Patched {pyproject_path} -> {YELLOW}{wheel_path.name}{RESET}")


def patch_mcp_json(folder: Path) -> None:
    """Rewrite all MCP config files so servers use `uv run apx` instead of bare `apx`."""
    # Known MCP config locations across assistant addons
    mcp_paths = [
        folder / ".mcp.json",           # claude
        folder / ".cursor" / "mcp.json", # cursor
        folder / ".vscode" / "mcp.json", # vscode
    ]

    patched = 0
    for mcp_path in mcp_paths:
        if not mcp_path.exists():
            continue

        data = json.loads(mcp_path.read_text())
        changed = False
        for _name, server in data.get("mcpServers", {}).items():
            if server.get("command") == "apx":
                server["command"] = "uv"
                server["args"] = ["run", "apx"] + server.get("args", [])
                changed = True

        if changed:
            mcp_path.write_text(json.dumps(data, indent=2) + "\n")
            rel = mcp_path.relative_to(folder)
            print(f"  Patched {rel} -> {YELLOW}uv run apx{RESET}")
            patched += 1

    if patched == 0:
        print(f"  {DIM}No MCP config files found to patch{RESET}")


def main() -> None:
    if len(sys.argv) < 3:
        print(
            f"{RED}Usage: uv run --script scripts/dev/gen.py <folder> <profile> [extra-args...]{RESET}",
            file=sys.stderr,
        )
        sys.exit(1)

    folder = Path(sys.argv[1])
    profile = sys.argv[2]
    extra_args = sys.argv[3:]

    project_root = Path(__file__).resolve().parent.parent.parent
    dist_dir = project_root / "dist"
    total = 7

    print(
        f"{BOLD}apx gen{RESET} — folder={YELLOW}{folder}{RESET} profile={YELLOW}{profile}{RESET}",
        end="",
    )
    if extra_args:
        print(f" args={YELLOW}{' '.join(extra_args)}{RESET}")
    else:
        print()

    t_total = time.monotonic()

    try:
        with stage("Building wheel", 1, total):
            run(["maturin", "build", "-j", "6", "-o", "dist"], cwd=project_root)

        wheel = find_wheel(dist_dir)
        print(f"  Wheel: {YELLOW}{wheel.name}{RESET}")

        with stage("Cleaning target folder", 2, total):
            if folder.exists():
                shutil.rmtree(folder)
                print(f"  Removed {folder}")
            else:
                print(f"  {DIM}Nothing to remove{RESET}")

        with stage("Initializing project", 3, total):
            run(
                [
                    "uvx",
                    "--no-cache",
                    "--from",
                    str(wheel),
                    "apx",
                    "init",
                    str(folder),
                    "-p",
                    profile,
                ]
                + extra_args,
                env={**os.environ, "RUST_LOG": "DEBUG"},
            )

        with stage("Patching pyproject.toml", 4, total):
            patch_pyproject(folder / "pyproject.toml", wheel)

        with stage("Patching MCP configs", 5, total):
            patch_mcp_json(folder)

        with stage("Syncing dependencies", 6, total):
            run(["uv", "sync", "--reinstall-package", "apx"], cwd=folder)

        with stage("Running dev check", 7, total):
            run(
                ["uv", "run", "apx", "dev", "check"],
                cwd=folder,
                env={**os.environ, "RUST_LOG": "DEBUG"},
            )

    except (RuntimeError, FileNotFoundError) as exc:
        print(f"\n{RED}{BOLD}Error:{RESET} {exc}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print(f"\n{YELLOW}Interrupted.{RESET}")
        sys.exit(130)

    elapsed = time.monotonic() - t_total
    print(
        f"\n{GREEN}{BOLD}All done!{RESET} {DIM}(total: {fmt_duration(elapsed)}){RESET}"
    )


if __name__ == "__main__":
    main()
