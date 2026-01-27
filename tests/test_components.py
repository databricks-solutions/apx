"""Test component imports are correctly rewritten."""

import json
import os
from pathlib import Path
import pytest
from conftest import run_cli_async, init_project_async


def get_cache_base_dir() -> Path:
    """Get the cache base directory, respecting APX_CACHE_DIR env var."""
    cache_dir = os.environ.get("APX_CACHE_DIR")
    if cache_dir:
        return Path(cache_dir)
    return Path.home() / ".apx" / "cache"


SAMPLE_PYPROJECT_TOML = """
[project]
name = "test-app"
dynamic = ["version"]
requires-python = ">=3.11"
dependencies = []

[tool.apx.metadata]
app-name = "Test App"
app-slug = "test_app"
app-entrypoint = "test_app.backend.app:app"
api-prefix = "/api"
metadata-path = "src/test_app/_metadata.py"

[tool.apx.ui]
root = "src/test_app/ui"

[tool.apx.ui.registries]
"@animate-ui" = "https://animate-ui.com/r/{name}.json"
"""


def setup_minimal_project(app_dir: Path) -> Path:
    """Set up a minimal project structure for component testing.

    Creates:
    - pyproject.toml with SAMPLE_PYPROJECT_TOML content
    - UI directory structure with styles/globals.css

    Args:
        app_dir: The directory to set up the project in

    Returns:
        Path to the UI root directory
    """
    (app_dir / "pyproject.toml").write_text(SAMPLE_PYPROJECT_TOML)
    ui_root = app_dir / "src" / "test_app" / "ui"
    ui_root.mkdir(parents=True)
    (ui_root / "styles").mkdir()
    (ui_root / "styles" / "globals.css").write_text("/* empty */")
    return ui_root


def check_file_imports(file_path: Path) -> list[str]:
    """Check if a file contains registry-prefixed imports."""
    if not file_path.exists():
        return []

    content = file_path.read_text()
    violations = []

    # Check for registry-prefixed imports
    if "@/registry/" in content:
        lines = content.split("\n")
        for i, line in enumerate(lines, 1):
            if "@/registry/" in line:
                violations.append(f"Line {i}: {line.strip()}")

    return violations


def find_import_violations(
    ui_root: Path, extensions: tuple[str, ...] = (".tsx", ".ts", ".jsx", ".js")
) -> list[str]:
    """Find all registry-prefixed import violations in the UI directory.

    Args:
        ui_root: The UI root directory to scan
        extensions: File extensions to check

    Returns:
        List of violation strings in format "relative/path: Line N: content"
    """
    violations = []
    for root, dirs, files in os.walk(ui_root):
        for file in files:
            if file.endswith(extensions):
                file_path = Path(root) / file
                file_violations = check_file_imports(file_path)
                if file_violations:
                    violations.extend(
                        [
                            f"{file_path.relative_to(ui_root)}: {v}"
                            for v in file_violations
                        ]
                    )
    return violations


@pytest.mark.parametrize("component_name", ["sidebar", "button", "card"])
async def test_component_import_rewriting(component_name: str, tmp_path: Path):
    """Test that each cached component's imports are correctly rewritten."""
    ui_root = setup_minimal_project(tmp_path)

    # Add the component
    result = await run_cli_async(
        ["components", "add", component_name, str(tmp_path)],
        cwd=tmp_path,
    )

    # Component might fail for various reasons (missing deps, network, etc.)
    # We only check import rewriting if the component was successfully added
    if result.returncode == 0:
        violations = find_import_violations(ui_root)
        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports that should be rewritten:\n"
            + "\n".join(violations)
        )


async def test_specific_known_components(tmp_path: Path):
    """Test specific components known to have registry imports."""
    known_components = ["sidebar", "button", "card"]
    ui_root = setup_minimal_project(tmp_path)

    for component_name in known_components:
        result = await run_cli_async(
            ["components", "add", component_name, str(tmp_path), "--force"],
            cwd=tmp_path,
        )

        # Verify it succeeded
        assert result.returncode == 0, (
            f"Failed to add {component_name}:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )

        # Check for registry-prefixed imports in all files
        violations = find_import_violations(ui_root, extensions=(".tsx", ".ts"))
        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports:\n"
            + "\n".join(violations)
        )


async def test_cache_population_structure(tmp_path: Path):
    """
    Test the structure and content of cached components.
    """
    # Initialize project
    result = await init_project_async(tmp_path)
    assert result.returncode == 0, f"Failed to initialize project: {result.stderr}"

    # Run add command
    result = await run_cli_async(
        ["components", "add", "button", str(tmp_path), "--force"],
        cwd=tmp_path,
    )
    assert result.returncode == 0, (
        f"Failed to add button: stdout={result.stdout}, stderr={result.stderr}"
    )

    # Check cache structure for a specific component
    cache_file = get_cache_base_dir() / "components" / "items" / "ui" / "button.json"

    if cache_file.exists():
        content = json.loads(cache_file.read_text())

        # Verify cache structure
        assert "version" in content, "Cache should have version"
        assert "fetched_at" in content, "Cache should have fetched_at timestamp"
        assert "item" in content, "Cache should have item (RegistryItem)"

        item = content["item"]
        assert "name" in item, "Item should have name"
        assert "files" in item, "Item should have files"
        assert item["name"] == "button", "Item name should be 'button'"

        print("\n=== Cached Component Structure ===")
        print(f"Version: {content['version']}")
        print(f"Fetched at: {content['fetched_at']}")
        print(f"Item name: {item['name']}")
        print(f"Files count: {len(item['files'])}")
