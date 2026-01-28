# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "typer"]
# ///

from __future__ import annotations

import hashlib
import io
import os
import re
import zipfile
from dataclasses import dataclass
from pathlib import Path

import httpx
import typer

DEFAULT_BUN_VERSION = "1.3.6"
BUN_RELEASES_BASE_URL = "https://github.com/oven-sh/bun/releases/download"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")


@dataclass(frozen=True, slots=True)
class BunAsset:
    platform: str
    arch: str
    filename: str

    @property
    def output_filename(self) -> str:
        suffix = ".exe" if self.platform == "windows" else ""
        return f"bun-{self.platform}-{self.arch}{suffix}"


BUN_ASSETS: tuple[BunAsset, ...] = (
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


def build_bun_url(version: str, asset: BunAsset) -> str:
    return f"{BUN_RELEASES_BASE_URL}/bun-v{version}/{asset.filename}"


def build_bun_shasums_url(version: str) -> str:
    return f"{BUN_RELEASES_BASE_URL}/bun-v{version}/SHASUMS256.txt"


def fetch_bun_shasums(client: httpx.Client, version: str) -> dict[str, str]:
    """Fetch and parse SHASUMS256.txt, returning {filename: sha256} mapping."""
    url = build_bun_shasums_url(version)
    typer.echo(f"fetch checksums: {url}")
    try:
        resp = client.get(url)
        resp.raise_for_status()
    except httpx.HTTPError as exc:
        typer.echo(f"error: failed to fetch checksums: {exc}", err=True)
        raise typer.Exit(code=1) from exc

    shasums: dict[str, str] = {}
    for line in resp.text.strip().splitlines():
        line = line.strip()
        if not line:
            continue
        # Format: "sha256  filename" (two spaces between)
        parts = line.split()
        if len(parts) >= 2:
            sha256_hash = parts[0]
            filename = parts[-1]
            shasums[filename] = sha256_hash
    return shasums


def compute_sha256(data: bytes) -> str:
    """Compute SHA256 hex digest of data."""
    return hashlib.sha256(data).hexdigest()


def verify_sha256(data: bytes, expected: str, filename: str) -> None:
    """Verify SHA256 of data matches expected, exit on mismatch."""
    actual = compute_sha256(data)
    if actual != expected:
        typer.echo(
            f"error: SHA256 mismatch for {filename}\n"
            f"  expected: {expected}\n"
            f"  actual:   {actual}",
            err=True,
        )
        raise typer.Exit(code=1)
    typer.echo(f"verified: {filename}")


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


def download_bun(
    client: httpx.Client,
    version: str,
    output_dir: Path,
    force: bool,
) -> None:
    """Download Bun binaries for all platforms."""
    typer.echo(f"\n=== Downloading Bun v{version} ===")
    bun_dir = output_dir / "bun"

    # Fetch checksums once for all assets
    shasums = fetch_bun_shasums(client, version)

    for asset in BUN_ASSETS:
        out_path = bun_dir / asset.output_filename
        if out_path.exists() and not force:
            typer.echo(f"skip: {out_path}")
            continue

        url = build_bun_url(version, asset)
        typer.echo(f"download: {url}")
        try:
            resp = client.get(url)
            resp.raise_for_status()
        except httpx.HTTPError as exc:
            raise typer.Exit(code=1) from exc

        # Verify SHA256 before extracting
        expected_sha = shasums.get(asset.filename)
        if expected_sha is None:
            typer.echo(f"error: no checksum found for {asset.filename}", err=True)
            raise typer.Exit(code=1)
        verify_sha256(resp.content, expected_sha, asset.filename)

        try:
            with zipfile.ZipFile(io.BytesIO(resp.content)) as zf:
                member = pick_bun_member(zf, prefer_exe=(asset.platform == "windows"))
                bun_bytes = zf.read(member)
        except (zipfile.BadZipFile, ValueError, KeyError) as exc:
            raise typer.Exit(code=1) from exc

        write_executable(out_path, bun_bytes)
        typer.echo(f"saved: {out_path}")


@app.command()
def main(
    bun_version: str = typer.Option(DEFAULT_BUN_VERSION, "--bun-version"),
    output_dir: Path = typer.Option(Path(".bins"), "--output-dir", "-o"),
    force: bool = typer.Option(False, "--force", "-f"),
) -> None:
    """
    Download Bun binaries for common platforms.

    Binaries are saved to:
      - .bins/bun/
    """
    bun_v = normalize_version(bun_version)
    output_dir = output_dir.expanduser().resolve()

    timeout = httpx.Timeout(connect=10.0, read=60.0, write=60.0, pool=10.0)
    with httpx.Client(follow_redirects=True, timeout=timeout) as client:
        download_bun(client, bun_v, output_dir, force)


if __name__ == "__main__":
    app()
