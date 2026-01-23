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


def test_generate_openapi_with_rich_operations(e2e_init: Path) -> None:
    """Test OpenAPI generation with a rich set of CRUD operations."""
    project_root = e2e_init
    src_dir = project_root / "src"
    backend_dir = src_dir / "base" / "backend"
    
    # Create a backend with rich REST operations
    backend_code = """from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

app = FastAPI(title='Rich API Example')

class Item(BaseModel):
    id: int
    name: str
    description: str | None = None
    price: float

class ItemCreate(BaseModel):
    name: str
    description: str | None = None
    price: float

class ItemUpdate(BaseModel):
    name: str | None = None
    description: str | None = None
    price: float | None = None

# Mock database
items_db: dict[int, Item] = {}

@app.get('/items', response_model=list[Item])
def list_items():
    '''List all items'''
    return list(items_db.values())

@app.get('/items/{item_id}', response_model=Item)
def get_item(item_id: int):
    '''Get a specific item by ID'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    return items_db[item_id]

@app.post('/items', response_model=Item, status_code=201)
def create_item(item: ItemCreate):
    '''Create a new item'''
    item_id = len(items_db) + 1
    new_item = Item(id=item_id, **item.model_dump())
    items_db[item_id] = new_item
    return new_item

@app.put('/items/{item_id}', response_model=Item)
def update_item(item_id: int, item: ItemCreate):
    '''Replace an entire item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    updated_item = Item(id=item_id, **item.model_dump())
    items_db[item_id] = updated_item
    return updated_item

@app.patch('/items/{item_id}', response_model=Item)
def partial_update_item(item_id: int, item: ItemUpdate):
    '''Partially update an item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    stored_item = items_db[item_id]
    update_data = item.model_dump(exclude_unset=True)
    updated_item = stored_item.model_copy(update=update_data)
    items_db[item_id] = updated_item
    return updated_item

@app.delete('/items/{item_id}', status_code=204)
def delete_item(item_id: int):
    '''Delete an item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    del items_db[item_id]
    return None
"""
    
    # Write the backend code
    (backend_dir / "app.py").write_text(backend_code)
    
    # Ensure .apx directory exists
    apx_dir = project_root / ".apx"
    apx_dir.mkdir(exist_ok=True)
    
    # Add src to path to allow imports
    sys.path.insert(0, str(src_dir))
    
    try:
        # Generate OpenAPI schema
        did_regenerate = generate_openapi(project_root, False)
        
        # Read and print the generated OpenAPI schema
        openapi_json_path = apx_dir / "openapi.json"
        if openapi_json_path.exists():
            openapi_schema = json.loads(openapi_json_path.read_text())
            print("\n" + "=" * 80)
            print("Generated OpenAPI Schema:")
            print("=" * 80)
            print(json.dumps(openapi_schema, indent=2))
            print("=" * 80)
            print(f"\nRegenerated: {did_regenerate}")
            print(f"Number of paths: {len(openapi_schema.get('paths', {}))}")
            print(f"Paths: {list(openapi_schema.get('paths', {}).keys())}")
            
            # Print operations by method
            paths = openapi_schema.get('paths', {})
            operations = {'get': [], 'post': [], 'put': [], 'patch': [], 'delete': []}
            for path, methods in paths.items():
                for method in ['get', 'post', 'put', 'patch', 'delete']:
                    if method in methods:
                        operations[method].append(path)
            
            print(f"\nOperations by method:")
            for method, paths_list in operations.items():
                if paths_list:
                    print(f"  {method.upper()}: {paths_list}")
        else:
            print("OpenAPI schema file not found!")
            
    finally:
        sys.path.remove(str(src_dir))
