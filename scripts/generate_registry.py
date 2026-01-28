# /// script
# requires-python = ">=3.11"
# dependencies = ["httpx", "jinja2"]
# ///

from collections import defaultdict
from pathlib import Path
import re

import httpx
from jinja2 import Environment, FileSystemLoader, select_autoescape

OWNER = "databricks-solutions"
REPO = "apx"

OUTPUT_ROOT = Path(".pages/simple")
TEMPLATES = Path("templates")

RELEASES_API = f"https://api.github.com/repos/{OWNER}/{REPO}/releases"

WHEEL_RE = re.compile(
    r"^(?P<name>[a-zA-Z0-9_]+)-(?P<version>[0-9][a-zA-Z0-9_.]*)-.*\.whl$"
)


def fetch_releases():
    with httpx.Client(timeout=30) as client:
        r = client.get(RELEASES_API)
        r.raise_for_status()
        return r.json()


def collect_wheels(releases):
    packages = defaultdict(list)

    for release in releases:
        for asset in release.get("assets", []):
            filename = asset["name"]
            if not filename.endswith(".whl"):
                continue

            m = WHEEL_RE.match(filename)
            if not m:
                continue

            pkg = m.group("name").lower()
            packages[pkg].append(
                {
                    "name": filename,
                    "url": asset["browser_download_url"],
                }
            )

    return packages


def main():
    env = Environment(
        loader=FileSystemLoader(TEMPLATES),
        autoescape=select_autoescape(["html"]),
    )

    root_tpl = env.get_template("root_index.html.jinja2")
    pkg_tpl = env.get_template("package_index.html.jinja2")

    releases = fetch_releases()
    packages = collect_wheels(releases)

    OUTPUT_ROOT.mkdir(parents=True, exist_ok=True)

    # Root index
    (OUTPUT_ROOT / "index.html").write_text(
        root_tpl.render(packages=sorted(packages)),
        encoding="utf-8",
    )

    # Per-package indexes
    for pkg, wheels in packages.items():
        pkg_dir = OUTPUT_ROOT / pkg
        pkg_dir.mkdir(exist_ok=True)

        (pkg_dir / "index.html").write_text(
            pkg_tpl.render(wheels=sorted(wheels, key=lambda w: w["name"])),
            encoding="utf-8",
        )


if __name__ == "__main__":
    main()
