from __future__ import annotations

import io
import os
import re
import zipfile
from dataclasses import dataclass
from pathlib import Path

import httpx
import typer

DEFAULT_BUN_VERSION = "1.3.5"
RELEASES_BASE_URL = "https://github.com/oven-sh/bun/releases/download"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")


@dataclass(frozen=True, slots=True)
class BunAsset:
    platform: str
    arch: str
    filename: str

    @property
    def url(self) -> str:
        raise RuntimeError("Use build_url(version, asset) instead.")

    @property
    def output_filename(self) -> str:
        suffix = ".exe" if self.platform == "windows" else ""
        return f"bun-{self.platform}-{self.arch}{suffix}"


ASSETS: tuple[BunAsset, ...] = (
    BunAsset(platform="windows", arch="x64", filename="bun-windows-x64.zip"),
    BunAsset(platform="linux", arch="x64", filename="bun-linux-x64.zip"),
    BunAsset(platform="linux", arch="aarch64", filename="bun-linux-aarch64.zip"),
    BunAsset(platform="darwin", arch="x64", filename="bun-darwin-x64.zip"),
    BunAsset(platform="darwin", arch="aarch64", filename="bun-darwin-aarch64.zip"),
)

app = typer.Typer(add_completion=False)


def normalize_version(version: str) -> str:
    v = version.strip()
    if v.startswith("v"):
        v = v[1:]
    if not VERSION_RE.fullmatch(v):
        raise typer.BadParameter("Expected version like 1.3.5")
    return v


def build_url(version: str, asset: BunAsset) -> str:
    return f"{RELEASES_BASE_URL}/bun-v{version}/{asset.filename}"


def pick_bun_member(zf: zipfile.ZipFile, *, prefer_exe: bool) -> zipfile.ZipInfo:
    candidates = [
        info
        for info in zf.infolist()
        if (not info.is_dir()) and Path(info.filename).name in {"bun", "bun.exe"}
    ]
    if not candidates:
        raise ValueError("Zip did not contain bun executable")

    preferred_name = "bun.exe" if prefer_exe else "bun"
    preferred = [c for c in candidates if Path(c.filename).name == preferred_name]
    if preferred:
        candidates = preferred

    return min(candidates, key=lambda i: len(i.filename))


def write_executable(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)

    if path.suffix != ".exe":
        current_mode = os.stat(path).st_mode
        os.chmod(path, current_mode | 0o111)


@app.command()
def main(
    version: str = typer.Option(DEFAULT_BUN_VERSION, "--version", "-v"),
    output_dir: Path = typer.Option(Path(".bins"), "--output-dir", "-o"),
    force: bool = typer.Option(False, "--force", "-f"),
) -> None:
    """
    Download Bun release binaries for common platforms into .bins/.
    """

    v = normalize_version(version)
    output_dir = output_dir.expanduser().resolve()

    timeout = httpx.Timeout(connect=10.0, read=60.0, write=60.0, pool=10.0)
    with httpx.Client(follow_redirects=True, timeout=timeout) as client:
        for asset in ASSETS:
            out_path = output_dir / asset.output_filename
            if out_path.exists() and not force:
                typer.echo(f"skip: {out_path}")
                continue

            url = build_url(v, asset)
            typer.echo(f"download: {url}")
            try:
                resp = client.get(url)
                resp.raise_for_status()
            except httpx.HTTPError as exc:
                raise typer.Exit(code=1) from exc

            try:
                with zipfile.ZipFile(io.BytesIO(resp.content)) as zf:
                    member = pick_bun_member(
                        zf, prefer_exe=(asset.platform == "windows")
                    )
                    bun_bytes = zf.read(member)
            except (zipfile.BadZipFile, ValueError, KeyError) as exc:
                raise typer.Exit(code=1) from exc

            write_executable(out_path, bun_bytes)
            typer.echo(f"saved: {out_path}")


if __name__ == "__main__":
    app()
