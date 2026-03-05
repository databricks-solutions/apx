"""Tests for apx._manifest module."""

from __future__ import annotations

import json
import sys
import os

import pytest

# Ensure src/ is importable
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))


@pytest.fixture
def simple_app():
    """Create a minimal FastAPI app with 2 routes."""
    from fastapi import FastAPI

    app = FastAPI(title="Test App")

    @app.get("/health")
    async def health():
        return {"status": "ok"}

    @app.post("/items")
    async def create_item(name: str):
        return {"name": name}

    return app


@pytest.fixture
def app_with_deps():
    """Create a FastAPI app with a dependency."""
    from fastapi import Depends, FastAPI

    app = FastAPI()

    async def get_db():
        return "db_connection"

    @app.get("/items")
    async def list_items(db=Depends(get_db)):
        return []

    return app


class TestCompileManifest:
    def test_compile_simple_app(self, simple_app, monkeypatch):
        """FastAPI app with 2 routes produces valid manifest dict."""
        from apx._manifest import compile_manifest

        # Patch importlib to return our test module
        import types
        fake_mod = types.ModuleType("test_app")
        fake_mod.app = simple_app
        monkeypatch.setitem(sys.modules, "test_app", fake_mod)

        manifest = compile_manifest("test_app")

        assert "routes" in manifest
        assert "meta" in manifest
        assert "dependency_graph" in manifest
        assert "lifecycle_deps" in manifest
        assert "max_body_limit" in manifest
        # 2 routes: GET /health, POST /items
        assert len(manifest["routes"]) == 2

        # Verify JSON-serializable
        json_str = json.dumps(manifest, default=str)
        parsed = json.loads(json_str)
        assert len(parsed["routes"]) == 2

    def test_compile_app_with_deps(self, app_with_deps, monkeypatch):
        """Route with Depends() gets AsgiBridge dispatch and CallPython steps."""
        from apx._manifest import compile_manifest

        import types
        fake_mod = types.ModuleType("test_deps_app")
        fake_mod.app = app_with_deps
        monkeypatch.setitem(sys.modules, "test_deps_app", fake_mod)

        manifest = compile_manifest("test_deps_app")
        routes = manifest["routes"]
        assert len(routes) == 1

        route = routes[0]
        assert route["dispatch_strategy"] == "AsgiBridge"
        assert route.get("dependency_plan") is not None

    def test_compile_app_dispatch_strategy(self, simple_app, monkeypatch):
        """Simple routes without deps get Direct dispatch."""
        from apx._manifest import compile_manifest

        import types
        fake_mod = types.ModuleType("test_dispatch_app")
        fake_mod.app = simple_app
        monkeypatch.setitem(sys.modules, "test_dispatch_app", fake_mod)

        manifest = compile_manifest("test_dispatch_app")
        for route in manifest["routes"]:
            assert route["dispatch_strategy"] == "Direct"

    def test_compile_app_openapi_included(self, simple_app, monkeypatch):
        """Manifest has openapi_schema populated."""
        from apx._manifest import compile_manifest

        import types
        fake_mod = types.ModuleType("test_openapi_app")
        fake_mod.app = simple_app
        monkeypatch.setitem(sys.modules, "test_openapi_app", fake_mod)

        manifest = compile_manifest("test_openapi_app")
        assert "openapi_schema" in manifest
        schema = manifest["openapi_schema"]
        assert "paths" in schema
        assert "openapi" in schema

    def test_compile_app_meta_populated(self, simple_app, monkeypatch):
        """Meta has version and timestamp fields."""
        from apx._manifest import compile_manifest

        import types
        fake_mod = types.ModuleType("test_meta_app")
        fake_mod.app = simple_app
        monkeypatch.setitem(sys.modules, "test_meta_app", fake_mod)

        manifest = compile_manifest("test_meta_app")
        meta = manifest["meta"]
        assert "apx_version" in meta
        assert "python_version" in meta
        assert "build_timestamp" in meta
        assert meta["app_module"] == "test_meta_app"
