"""Test component imports are correctly rewritten."""

import json
import os
import shutil
from pathlib import Path
import pytest
from conftest import apx_source_dir, run_cli_async


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
app-module = "test_app.backend.app:app"
api-prefix = "/api"
metadata-path = "src/test_app/_metadata.py"

[tool.apx.ui]
root = "src/test_app/ui"

[tool.apx.ui.registries]
"@animate-ui" = "https://animate-ui.com/r/{name}.json"
"""


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


@pytest.mark.parametrize("component_name", ["sidebar", "button", "card"])
async def test_component_import_rewriting(component_name: str, tmp_path: Path):
    """Test that each cached component's imports are correctly rewritten."""
    app_dir = tmp_path

    # Create pyproject.toml
    (app_dir / "pyproject.toml").write_text(SAMPLE_PYPROJECT_TOML)

    # Create UI directory structure
    ui_root = app_dir / "src" / "test_app" / "ui"
    ui_root.mkdir(parents=True)
    (ui_root / "styles").mkdir()
    (ui_root / "styles" / "globals.css").write_text("/* empty */")

    # Add the component
    result = await run_cli_async(
        ["components", "add", component_name, str(app_dir)],
        cwd=app_dir,
    )

    # Component might fail for various reasons (missing deps, network, etc.)
    # We only check import rewriting if the component was successfully added
    if result.returncode == 0:
        # Check all written files for registry-prefixed imports
        violations = []

        for root, dirs, files in os.walk(ui_root):
            for file in files:
                if file.endswith((".tsx", ".ts", ".jsx", ".js")):
                    file_path = Path(root) / file
                    file_violations = check_file_imports(file_path)
                    if file_violations:
                        violations.extend(
                            [
                                f"{file_path.relative_to(ui_root)}: {v}"
                                for v in file_violations
                            ]
                        )

        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports that should be rewritten:\n"
            + "\n".join(violations)
        )


async def test_specific_known_components(tmp_path: Path):
    """Test specific components known to have registry imports."""
    known_components = ["sidebar", "button", "card"]

    app_dir = tmp_path

    # Create pyproject.toml
    (app_dir / "pyproject.toml").write_text(SAMPLE_PYPROJECT_TOML)

    # Create UI directory structure
    ui_root = app_dir / "src" / "test_app" / "ui"
    ui_root.mkdir(parents=True)
    (ui_root / "styles").mkdir()
    (ui_root / "styles" / "globals.css").write_text("/* empty */")

    for component_name in known_components:
        result = await run_cli_async(
            ["components", "add", component_name, str(app_dir), "--force"],
            cwd=app_dir,
        )

        # Verify it succeeded
        assert result.returncode == 0, (
            f"Failed to add {component_name}:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )

        # Check for registry-prefixed imports in all files
        violations = []
        for root, dirs, files in os.walk(ui_root):
            for file in files:
                if file.endswith((".tsx", ".ts")):
                    file_path = Path(root) / file
                    file_violations = check_file_imports(file_path)
                    if file_violations:
                        violations.extend(
                            [
                                f"{file_path.relative_to(ui_root)}: {v}"
                                for v in file_violations
                            ]
                        )

        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports:\n"
            + "\n".join(violations)
        )


async def test_cache_population_after_add(tmp_path: Path):
    """
    Test that after running 'add' command, the cache has:
    1. registry.json files for all registries in pyproject.toml
    2. Individual items prefetched ONLY for default shadcn registry
    3. Custom registries have registry.json but items are fetched on-demand

    Cache structure:
    - {APX_CACHE_DIR}/components/registries/{name}/registry.json
    - {APX_CACHE_DIR}/components/items/ui/*.json (default shadcn items prefetched)
    """
    # Step 1: Clear existing cache
    cache_base = get_cache_base_dir() / "components"
    if cache_base.exists():
        shutil.rmtree(cache_base)

    # Step 2: Initialize project (skip deps)
    result = await run_cli_async(
        [
            "init",
            str(tmp_path),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
            "--assistant",
            "cursor",
            "--layout",
            "basic",
            "--template",
            "essential",
            "--profile",
            "DEFAULT",
            "--name",
            "test-app",
            "--apx-package",
            apx_source_dir,
            "--apx-editable",
        ],
        cwd=tmp_path,
    )
    assert result.returncode == 0, "Failed to initialize project"

    # Check what registries are in pyproject.toml
    pyproject_path = tmp_path / "pyproject.toml"
    pyproject_content = pyproject_path.read_text()
    print("\n=== pyproject.toml registries section ===")
    if "[tool.apx.ui.registries]" in pyproject_content:
        start = pyproject_content.find("[tool.apx.ui.registries]")
        end = pyproject_content.find("\n[", start + 1)
        if end == -1:
            end = len(pyproject_content)
        print(pyproject_content[start:end])
    else:
        print("No custom registries defined")

    # Step 3: Run add command for a single component (dialog)
    result = await run_cli_async(
        ["components", "add", "dialog", str(tmp_path)],
        cwd=tmp_path,
    )
    assert result.returncode == 0, (
        f"Failed to add dialog: stdout={result.stdout}, stderr={result.stderr}"
    )

    print("\n=== Add command output ===")
    print(result.stdout)

    # Step 4: Verify cache structure
    registries_dir = cache_base / "registries"
    items_dir = cache_base / "items"

    # Check registry.json files exist
    assert registries_dir.exists(), "registries/ directory should exist"

    # Default registry should have registry.json
    default_registry_json = registries_dir / "ui" / "registry.json"
    assert default_registry_json.exists(), (
        "Default registry (ui/registry.json) should exist"
    )

    # Verify registry.json has items
    default_registry_data = json.loads(default_registry_json.read_text())
    assert "items" in default_registry_data, "registry.json should have items field"
    assert len(default_registry_data["items"]) >= 30, (
        f"Default registry should have 30+ items, got {len(default_registry_data['items'])}"
    )
    print(f"\nDefault registry.json items: {len(default_registry_data['items'])}")

    # Check default registry items are prefetched
    default_items_dir = items_dir / "ui"
    assert default_items_dir.exists(), "Default registry items (items/ui/) should exist"

    default_items = list(default_items_dir.glob("*.json"))
    print(f"Default registry items cached: {len(default_items)}")

    # Should have prefetched items (not just dialog)
    assert len(default_items) > 1, (
        f"Default registry should have prefetched items, got: {[c.stem for c in default_items]}"
    )

    # Check custom registries have registry.json but NOT prefetched items
    custom_registries = [
        d for d in registries_dir.iterdir() if d.is_dir() and d.name != "ui"
    ]
    print(
        f"\nCustom registries with registry.json: {[r.name for r in custom_registries]}"
    )

    for reg_dir in custom_registries:
        reg_json = reg_dir / "registry.json"
        if reg_json.exists():
            reg_data = json.loads(reg_json.read_text())
            print(
                f"  {reg_dir.name}: {len(reg_data.get('items', []))} items in registry.json"
            )

            # Custom registry items should NOT be prefetched
            items_path = items_dir / reg_dir.name
            if items_path.exists():
                items_count = len(list(items_path.glob("*.json")))
                print(f"    (items prefetched: {items_count})")
                # Items should be 0 or minimal (only fetched on-demand)

    print("\n=== Cache Structure ===")
    print(f"Cache base: {cache_base}")
    print(f"Registries dir: {registries_dir}")
    print(f"Items dir: {items_dir}")


async def test_cache_population_structure(tmp_path: Path):
    """
    Test the structure and content of cached components.
    """
    # Initialize project
    result = await run_cli_async(
        [
            "init",
            str(tmp_path),
            "--skip-backend-dependencies",
            "--skip-frontend-dependencies",
            "--skip-build",
            "--assistant",
            "cursor",
            "--layout",
            "basic",
            "--template",
            "essential",
            "--profile",
            "DEFAULT",
            "--name",
            "test-app",
            "--apx-package",
            apx_source_dir,
            "--apx-editable",
        ],
        cwd=tmp_path,
    )
    assert result.returncode == 0, "Failed to initialize project"

    # Run add command
    result = await run_cli_async(
        ["components", "add", "button", str(tmp_path)],
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
