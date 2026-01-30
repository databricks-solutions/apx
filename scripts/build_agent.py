# /// script
# requires-python = ">=3.11"
# dependencies = ["typer"]
# ///

"""
Build apx-agent binary for all supported platforms.

This script builds the apx-agent binary using cargo for the current platform,
and optionally uses cross-rs for cross-compilation to other platforms.

Usage:
    uv run scripts/build_agent.py              # Build for current platform
    uv run scripts/build_agent.py --all        # Build for all platforms
    uv run scripts/build_agent.py --target x86_64-unknown-linux-gnu
"""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

import typer

app = typer.Typer(add_completion=False)


@dataclass(frozen=True, slots=True)
class Target:
    rust_target: str
    platform: str
    arch: str
    needs_cross: bool  # True if cross-rs is needed

    @property
    def output_filename(self) -> str:
        suffix = ".exe" if self.platform == "windows" else ""
        return f"apx-agent-{self.platform}-{self.arch}{suffix}"

    @property
    def binary_name(self) -> str:
        return "apx-agent.exe" if self.platform == "windows" else "apx-agent"


# All supported targets
ALL_TARGETS: tuple[Target, ...] = (
    Target(
        rust_target="aarch64-apple-darwin",
        platform="darwin",
        arch="aarch64",
        needs_cross=False,  # Native on macOS ARM
    ),
    Target(
        rust_target="x86_64-apple-darwin",
        platform="darwin",
        arch="x64",
        needs_cross=False,  # Can cross-compile on macOS
    ),
    Target(
        rust_target="aarch64-unknown-linux-gnu",
        platform="linux",
        arch="aarch64",
        needs_cross=True,
    ),
    Target(
        rust_target="x86_64-unknown-linux-gnu",
        platform="linux",
        arch="x64",
        needs_cross=True,
    ),
    Target(
        rust_target="x86_64-pc-windows-gnu",
        platform="windows",
        arch="x64",
        needs_cross=True,
    ),
    Target(
        rust_target="x86_64-pc-windows-msvc",
        platform="windows",
        arch="x64",
        needs_cross=False,  # Native on Windows
    ),
)


def get_current_target() -> Target | None:
    """Determine the current host target."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    # Normalize architecture names
    if machine in ("arm64", "aarch64"):
        arch = "aarch64"
    elif machine in ("x86_64", "amd64"):
        arch = "x64"
    else:
        return None

    # Normalize platform names and determine rust target
    if system == "darwin":
        plat = "darwin"
        rust_target = f"{arch.replace('x64', 'x86_64')}-apple-darwin"
    elif system == "linux":
        plat = "linux"
        rust_target = f"{arch.replace('x64', 'x86_64')}-unknown-linux-gnu"
    elif system == "windows":
        plat = "windows"
        # Use MSVC target on native Windows
        rust_target = "x86_64-pc-windows-msvc"
    else:
        return None

    # Find matching target by rust_target (more precise)
    for target in ALL_TARGETS:
        if target.rust_target == rust_target:
            return target

    # Fallback: find by platform and arch
    for target in ALL_TARGETS:
        if target.platform == plat and target.arch == arch and not target.needs_cross:
            return target
    return None


def build_target(target: Target, output_dir: Path, release: bool = True) -> None:
    """Build apx-agent for a specific target."""
    typer.echo(f"\n=== Building for {target.rust_target} ===")

    # Determine build tool
    # Skip cross if we're building for the current host platform
    current = get_current_target()
    needs_cross = target.needs_cross and (current is None or target != current)

    if needs_cross:
        # Check if cross is available
        if shutil.which("cross") is None:
            typer.echo(
                "error: 'cross' is required for cross-compilation. "
                "Install with: cargo install cross",
                err=True,
            )
            raise typer.Exit(code=1)
        build_cmd = "cross"
    else:
        build_cmd = "cargo"

    # Build command
    cmd = [
        build_cmd,
        "build",
        "-p",
        "apx-agent",
        "--target",
        target.rust_target,
    ]
    if release:
        cmd.append("--release")

    typer.echo(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=Path.cwd())
    if result.returncode != 0:
        typer.echo(f"error: build failed for {target.rust_target}", err=True)
        raise typer.Exit(code=1)

    # Copy binary to output directory
    profile = "release" if release else "debug"
    built_binary = Path("target") / target.rust_target / profile / target.binary_name

    if not built_binary.exists():
        typer.echo(f"error: built binary not found at {built_binary}", err=True)
        raise typer.Exit(code=1)

    output_dir.mkdir(parents=True, exist_ok=True)
    dest = output_dir / target.output_filename
    shutil.copy2(built_binary, dest)

    # Set executable permissions on Unix
    if not dest.suffix == ".exe":
        current_mode = os.stat(dest).st_mode
        os.chmod(dest, current_mode | 0o111)

    typer.echo(f"Saved: {dest}")


def find_target(target_name: str) -> Target | None:
    """Find target by rust target name or platform-arch."""
    for target in ALL_TARGETS:
        if target.rust_target == target_name:
            return target
        if f"{target.platform}-{target.arch}" == target_name:
            return target
    return None


@app.command()
def main(
    target: str | None = typer.Option(
        None,
        "--target",
        "-t",
        help="Specific target to build (e.g., x86_64-unknown-linux-gnu or darwin-aarch64)",
    ),
    all_targets: bool = typer.Option(
        False,
        "--all",
        "-a",
        help="Build for all supported platforms",
    ),
    output_dir: Path = typer.Option(
        Path(".bins/agent"),
        "--output-dir",
        "-o",
        help="Output directory for binaries",
    ),
    debug: bool = typer.Option(
        False,
        "--debug",
        help="Build in debug mode instead of release",
    ),
) -> None:
    """
    Build apx-agent binary for specified platforms.

    By default, builds for the current platform only.
    Use --all to build for all supported platforms.
    """
    output_dir = output_dir.expanduser().resolve()

    if all_targets:
        # Build all targets
        targets = ALL_TARGETS
        typer.echo(f"Building apx-agent for {len(targets)} platforms...")
    elif target:
        # Build specific target
        t = find_target(target)
        if t is None:
            typer.echo(f"error: unknown target '{target}'", err=True)
            typer.echo("Available targets:")
            for t in ALL_TARGETS:
                typer.echo(f"  - {t.rust_target} ({t.platform}-{t.arch})")
            raise typer.Exit(code=1)
        targets = (t,)
    else:
        # Build for current platform
        current = get_current_target()
        if current is None:
            typer.echo("error: could not determine current platform", err=True)
            raise typer.Exit(code=1)
        targets = (current,)
        typer.echo(f"Building apx-agent for current platform: {current.rust_target}")

    release = not debug
    for t in targets:
        build_target(t, output_dir, release)

    typer.echo(f"\n=== Done! Binaries saved to {output_dir} ===")
    for f in sorted(output_dir.iterdir()):
        if f.is_file():
            size_kb = f.stat().st_size / 1024
            typer.echo(f"  {f.name} ({size_kb:.1f} KB)")


if __name__ == "__main__":
    app()
