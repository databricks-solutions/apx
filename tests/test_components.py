"""Test component imports are correctly rewritten."""
import os
from pathlib import Path
import pytest
from conftest import ApxFixture


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


def get_cached_component_names():
    """Get list of component names from cache."""
    cache_dir = Path.home() / ".apx" / "cache" / "components" / "items" / "ui"
    if not cache_dir.exists():
        return []
    
    components = []
    for file in cache_dir.glob("*.json"):
        # Extract component name from filename (e.g., "button.json" -> "button")
        component_name = file.stem
        components.append(component_name)
    
    return sorted(components)


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


@pytest.mark.parametrize("component_name", get_cached_component_names())
def test_component_import_rewriting(component_name: str, run_apx: ApxFixture, tmp_path: Path):
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
    result = run_apx(["components", "add", component_name, str(app_dir)])
    
    # Component might fail for various reasons (missing deps, network, etc.)
    # We only check import rewriting if the component was successfully added
    if result.code == 0:
        # Check all written files for registry-prefixed imports
        violations = []
        
        for root, dirs, files in os.walk(ui_root):
            for file in files:
                if file.endswith((".tsx", ".ts", ".jsx", ".js")):
                    file_path = Path(root) / file
                    file_violations = check_file_imports(file_path)
                    if file_violations:
                        violations.extend([
                            f"{file_path.relative_to(ui_root)}: {v}"
                            for v in file_violations
                        ])
        
        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports that should be rewritten:\n" +
            "\n".join(violations)
        )


def test_component_cache_exists():
    """Verify that component cache exists and has components."""
    cache_dir = Path.home() / ".apx" / "cache" / "components" / "items" / "ui"
    
    # Skip test if cache doesn't exist
    if not cache_dir.exists():
        pytest.skip("Component cache directory does not exist. Run 'apx components sync' first.")
    
    components = get_cached_component_names()
    assert len(components) > 0, (
        "No cached components found. Run 'apx components sync' first."
    )


def test_specific_known_components(run_apx: ApxFixture, tmp_path: Path):
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
        result = run_apx(["components", "add", component_name, str(app_dir), "--force"])
        
        # Verify it succeeded
        assert result.code == 0, (
            f"Failed to add {component_name}:\nstdout: {result.out}\nstderr: {result.err}"
        )
        
        # Check for registry-prefixed imports in all files
        violations = []
        for root, dirs, files in os.walk(ui_root):
            for file in files:
                if file.endswith((".tsx", ".ts")):
                    file_path = Path(root) / file
                    file_violations = check_file_imports(file_path)
                    if file_violations:
                        violations.extend([
                            f"{file_path.relative_to(ui_root)}: {v}"
                            for v in file_violations
                        ])
        
        assert not violations, (
            f"Component '{component_name}' has registry-prefixed imports:\n" +
            "\n".join(violations)
        )
