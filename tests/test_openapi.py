import json
import sys
from importlib import import_module
from pathlib import Path

from fastapi import FastAPI

from apx._core import generate_openapi


def test_generate_openapi_skips_orval_when_schema_unchanged(tmp_path: Path) -> None:
    app_slug = "test_app"
    app_module = "test_app.backend.app:app"
    project_root = tmp_path
    src_dir = project_root / "src"
    backend_dir = src_dir / app_slug / "backend"
    backend_dir.mkdir(parents=True)
    (src_dir / app_slug / "__init__.py").write_text("")
    (backend_dir / "__init__.py").write_text("")
    (backend_dir / "app.py").write_text(
        "\n".join(
            [
                "from fastapi import FastAPI",
                "",
                "app = FastAPI()",
                "",
                "@app.get('/ping')",
                "def ping():",
                "    return {'status': 'ok'}",
                "",
            ]
        )
    )

    pyproject = "\n".join(
        [
            "[tool.apx.metadata]",
            'app-name = "Test App"',
            f'app-module = "{app_module}"',
            f'app-slug = "{app_slug}"',
            'api-prefix = "/api"',
            'metadata-path = "src/test_app/backend/_metadata.py"',
            "",
        ]
    )
    (project_root / "pyproject.toml").write_text(pyproject)

    sys.path.insert(0, str(project_root))
    sys.path.insert(0, str(project_root / "src"))
    try:
        module = import_module("test_app.backend.app")
        app = getattr(module, "app")  # pyright: ignore[reportAny]
        assert isinstance(app, FastAPI)
        expected_json = json.dumps(app.openapi(), indent=2)
    finally:
        sys.path.remove(str(project_root))

    apx_dir = project_root / ".apx"
    apx_dir.mkdir()
    (apx_dir / "openapi.json").write_text(expected_json)

    did_regenerate = generate_openapi(project_root, False)

    assert did_regenerate is False
    assert (apx_dir / "openapi.json").read_text() == expected_json
    assert (apx_dir / "orval.config.ts").exists()
