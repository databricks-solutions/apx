"""Tests for the Rust MCP server implementation."""

import json
import os
from contextlib import asynccontextmanager
from pathlib import Path

from mcp.types import TextResourceContents, CallToolResult, TextContent, Tool, Resource
from pydantic import AnyUrl
import pytest
from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client


# =============================================================================
# MCP Session Helpers
# =============================================================================


@asynccontextmanager
async def mcp_session(project_dir: Path | None = None):
    """Context manager for MCP client session.

    Args:
        project_dir: Optional directory to change to before starting the session.
                     If None, uses current directory.
    """
    original_cwd = os.getcwd()
    try:
        if project_dir is not None:
            os.chdir(project_dir)
        _env = os.environ.copy()
        _env["APX_LOG"] = "debug"
        server_params = StdioServerParameters(
            command="uv",
            args=["run", "--no-sync", "apx", "mcp"],
            env=_env,
        )
        async with stdio_client(server_params) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                yield session
    finally:
        os.chdir(original_cwd)


# =============================================================================
# Result Parsing Helpers
# =============================================================================


def retrieve_text_content(result: CallToolResult) -> str:
    """Retrieve text content from MCP tool result."""
    assert len(result.content) > 0, "Result should have content"
    _extracted = result.content[0]
    assert isinstance(_extracted, TextContent), "Result should have text content"
    return _extracted.text


def parse_json_result(result: CallToolResult):
    """Parse JSON from MCP tool result."""
    assert len(result.content) > 0, "Result should have content"
    result_text = retrieve_text_content(result)
    try:
        return json.loads(result_text)
    except json.JSONDecodeError as e:
        raise ValueError(f"Failed to parse JSON: {e} from {result_text}") from e


# =============================================================================
# Assertion Helpers
# =============================================================================


async def assert_tool_exists(session: ClientSession, tool_name: str) -> Tool:
    """Assert a tool exists and return its metadata."""
    tools = await session.list_tools()
    tool_names = [t.name for t in tools.tools]
    assert tool_name in tool_names, f"Tool '{tool_name}' should exist"
    return next(t for t in tools.tools if t.name == tool_name)


async def assert_resource_exists(session: ClientSession, uri: str) -> Resource:
    """Assert a resource exists and return its metadata."""
    resources = await session.list_resources()
    uris = [str(r.uri) for r in resources.resources]
    assert uri in uris, f"Resource '{uri}' should exist"
    return next(r for r in resources.resources if str(r.uri) == uri)


def skip_if_sdk_unavailable(result_text: str):
    """Skip test if Databricks SDK is not available."""
    if "not available" in result_text or "not installed" in result_text:
        pytest.skip("Databricks SDK not installed or docs not indexed")


async def test_apx_info_resource(common_project: Path):
    """Test that the Rust MCP server provides the apx://info resource."""
    async with mcp_session(common_project) as session:
        # Verify resource exists
        apx_info_resource = await assert_resource_exists(session, "apx://info")
        assert apx_info_resource.name == "apx-info"
        assert apx_info_resource.description is not None
        assert "apx toolkit" in apx_info_resource.description.lower()
        assert apx_info_resource.mimeType == "text/plain"

        # Read and validate content
        content = await session.read_resource(AnyUrl("apx://info"))
        assert len(content.contents) == 1
        contents = content.contents[0]
        assert isinstance(contents, TextResourceContents)
        assert contents.text is not None
        assert "apx" in contents.text.lower()
        assert "Databricks app" in contents.text
        assert "Technology Stack" in contents.text


async def test_start_tool_exists(common_project: Path):
    """Test that the start tool is available."""
    async with mcp_session(common_project) as session:
        start_tool = await assert_tool_exists(session, "start")
        assert start_tool.description is not None
        assert "start development server" in start_tool.description.lower()
        assert "inputSchema" in dir(start_tool)


async def test_mcp_server_capabilities(common_project: Path):
    """Test that the MCP server advertises correct capabilities."""
    original_cwd = os.getcwd()
    try:
        os.chdir(common_project)
        server_params = StdioServerParameters(
            command="uv", args=["run", "--no-sync", "apx", "mcp"]
        )

        async with stdio_client(server_params) as (read, write):
            async with ClientSession(read, write) as session:
                init_result = await session.initialize()

                # Check protocol version
                assert init_result.protocolVersion == "2024-11-05"

                # Check server info
                assert init_result.serverInfo is not None
                assert init_result.serverInfo.name == "apx"
                assert init_result.serverInfo.version is not None

                # Check capabilities
                assert init_result.capabilities is not None
                assert hasattr(init_result.capabilities, "resources")
                assert hasattr(init_result.capabilities, "tools")
    finally:
        os.chdir(original_cwd)


async def test_search_registry_components(common_project: Path):
    """Test search_registry_components tool (read-only)."""
    async with mcp_session(common_project) as session:
        # Verify tools exist
        await assert_tool_exists(session, "search_registry_components")
        await assert_tool_exists(session, "add_component")

        # Search for button component
        search_result = await session.call_tool(
            "search_registry_components",
            arguments={"query": "button component for clicking", "limit": 5},
        )
        result_json = parse_json_result(search_result)

        assert result_json["query"] == "button component for clicking"
        results = result_json["results"]

        # Verify "button" is in the results with high similarity
        result_ids = [r["id"] for r in results]
        assert "button" in result_ids, "Button should be in search results"
        button_result = next(r for r in results if r["id"] == "button")
        assert button_result["score"] > 0.85, "Button should have high similarity score"


async def test_search_custom_registry_components(common_project: Path):
    """Test search for custom registry components (read-only)."""
    async with mcp_session(common_project) as session:
        # Search for sidebar component from animate-ui
        search_result = await session.call_tool(
            "search_registry_components",
            arguments={"query": "animated sidebar navigation component", "limit": 5},
        )
        result_json = parse_json_result(search_result)
        results = result_json["results"]

        # Verify we got results from default registry
        default_results = [r for r in results if not r.get("id", "").startswith("@")]
        assert len(default_results) > 0, "Should have results from default registry"


async def test_add_component(isolated_project: Path):
    """Test add_component tool (modifies project)."""
    async with mcp_session(isolated_project) as session:
        add_result = await session.call_tool(
            "add_component", arguments={"component_id": "dialog", "force": False}
        )
        result_text = retrieve_text_content(add_result)

        # Verify component was created if successful
        if "Successfully added component" in result_text:
            dialog_file = (
                isolated_project
                / "src"
                / "test_app"
                / "ui"
                / "components"
                / "ui"
                / "dialog.tsx"
            )
            assert dialog_file.exists(), "Dialog component file should be created"
            content = dialog_file.read_text()
            assert len(content) > 0, "Dialog component should have content"
            assert "dialog" in content.lower()


async def test_add_custom_registry_component(isolated_project: Path):
    """Test add_component for custom registry (modifies project)."""
    async with mcp_session(isolated_project) as session:
        add_result = await session.call_tool(
            "add_component",
            arguments={
                "component_id": "@animate-ui/components-radix-sidebar",
                "force": False,
            },
        )
        result_text = retrieve_text_content(add_result)

        sidebar_file = (
            isolated_project
            / "src"
            / "test_app"
            / "ui"
            / "components"
            / "animate-ui"
            / "components-radix-sidebar.tsx"
        )

        if "Successfully added component" in result_text:
            assert sidebar_file.exists(), "Sidebar component should be created"
            content = sidebar_file.read_text()
            assert len(content) > 0, "Sidebar component should have content"
            assert any(
                term in content.lower() for term in ["sidebar", "navigation", "nav"]
            ), "Component should contain sidebar/navigation related content"
        elif "Failed to add component" in result_text:
            # Acceptable failure modes for external registry
            expected_errors = [
                "Failed to fetch",
                "Registry returned error",
                "Unknown registry",
                "404",
                "File already exists",
                "Failed to",
            ]
            assert any(term in result_text for term in expected_errors), (
                "Should have a clear error message"
            )


async def test_docs_tool(common_project: Path):
    """Test the docs tool for searching Databricks SDK documentation."""
    async with mcp_session(common_project) as session:
        await assert_tool_exists(session, "docs")

        # Search for cluster-related docs
        search_result = await session.call_tool(
            "docs",
            arguments={
                "source": "databricks-sdk-python",
                "query": "create cluster",
                "num_results": 3,
            },
        )
        result_text = retrieve_text_content(search_result)
        skip_if_sdk_unavailable(result_text)

        # Parse and validate results
        result_json = json.loads(result_text)
        assert result_json["source"] == "databricks-sdk-python"
        assert result_json["query"] == "create cluster"

        results = result_json["results"]
        assert len(results) <= 3, "Should respect num_results limit"

        if len(results) > 0:
            first_result = results[0]
            assert "text" in first_result and len(first_result["text"]) > 0
            assert "source_file" in first_result
            assert "score" in first_result and 0 <= first_result["score"] <= 1
            assert "workspace" in first_result["source_file"]


async def test_docs_create_cluster(common_project: Path):
    """Test that 'create cluster' query returns relevant cluster creation docs in top 3."""
    async with mcp_session(common_project) as session:
        result = await session.call_tool(
            "docs",
            arguments={
                "source": "databricks-sdk-python",
                "query": "create cluster",
                "num_results": 5,
            },
        )
        result_text = retrieve_text_content(result)
        skip_if_sdk_unavailable(result_text)

        result_json = parse_json_result(result)
        results = result_json["results"]

        # At least one of top 3 should be cluster-related
        top_3 = results[:3]
        cluster_related = [
            r
            for r in top_3
            if "cluster" in r["source_file"].lower() or "cluster" in r["text"].lower()
        ]

        assert len(cluster_related) >= 1, (
            f"Expected at least 1 cluster-related result in top 3, got {len(cluster_related)}. "
            f"Top 3 files: {[r['source_file'] for r in top_3]}"
        )


ROUTER_CODE_TEMPLATE = """from fastapi import APIRouter, HTTPException
from pydantic import BaseModel
from .._metadata import api_prefix

api = APIRouter(prefix=api_prefix)

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

@api.get('/items', response_model=list[Item], operation_id='listItems')
def list_items():
    '''List all items'''
    return list(items_db.values())

@api.get('/items/{item_id}', response_model=Item, operation_id='getItem')
def get_item(item_id: int):
    '''Get a specific item by ID'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    return items_db[item_id]

@api.post('/items', response_model=Item, status_code=201, operation_id='createItem')
def create_item(item: ItemCreate):
    '''Create a new item'''
    item_id = len(items_db) + 1
    new_item = Item(id=item_id, **item.model_dump())
    items_db[item_id] = new_item
    return new_item

@api.put('/items/{item_id}', response_model=Item, operation_id='updateItem')
def update_item(item_id: int, item: ItemCreate):
    '''Replace an entire item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    updated_item = Item(id=item_id, **item.model_dump())
    items_db[item_id] = updated_item
    return updated_item

@api.patch('/items/{item_id}', response_model=Item, operation_id='partialUpdateItem')
def partial_update_item(item_id: int, item: ItemUpdate):
    '''Partially update an item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    stored_item = items_db[item_id]
    update_data = item.model_dump(exclude_unset=True)
    updated_item = stored_item.model_copy(update=update_data)
    items_db[item_id] = updated_item
    return updated_item

@api.delete('/items/{item_id}', status_code=204, operation_id='deleteItem')
def delete_item(item_id: int):
    '''Delete an item'''
    if item_id not in items_db:
        raise HTTPException(status_code=404, detail='Item not found')
    del items_db[item_id]
    return None
"""


async def _setup_openapi_project(project_path: Path) -> None:
    """Set up a project with router code and generate OpenAPI."""
    from conftest import run_cli_async

    src_dir = project_path / "src"
    backend_dir = src_dir / "test_app" / "backend"

    (backend_dir / "router.py").write_text(ROUTER_CODE_TEMPLATE)
    (src_dir / "test_app" / "_version.py").write_text('version = "0.0.0"\n')

    result = await run_cli_async(
        ["__generate_openapi", "--app-dir", str(project_path)],
        cwd=project_path,
    )
    assert result.returncode == 0, "OpenAPI generation should succeed"


def _assert_query_example(example: str, operation_name: str) -> None:
    """Assert that a query example contains expected patterns."""
    assert f'import {{ use{operation_name} }} from "@/lib/api"' in example
    assert 'import selector from "@/lib/selector"' in example
    assert (
        f"const {{ data, isLoading, error }} = use{operation_name}(selector())"
        in example
    )
    assert "if (isLoading)" in example
    assert "if (error)" in example
    # Suspense hook
    assert f'import {{ use{operation_name}Suspense }} from "@/lib/api"' in example
    assert f"const {{ data }} = use{operation_name}Suspense(selector())" in example
    # Type hints
    assert f"{operation_name}QueryResult" in example
    assert f"{operation_name}QueryError" in example


def _assert_mutation_example(example: str, operation_name: str) -> None:
    """Assert that a mutation example contains expected patterns."""
    assert f'import {{ use{operation_name} }} from "@/lib/api"' in example
    assert f"const {{ mutate, isPending }} = use{operation_name}()" in example
    assert "mutate({ data: { /* request body */ } })" in example
    assert "onClick={handleSubmit}" in example
    # Type hints
    assert f"{operation_name}MutationBody" in example
    assert f"{operation_name}MutationResult" in example
    assert f"{operation_name}MutationError" in example


async def test_routes_resource_and_get_route_info(isolated_project: Path):
    """Test routes resource and get_route_info tool with generated OpenAPI."""
    await _setup_openapi_project(isolated_project)

    async with mcp_session(isolated_project) as session:
        # Verify routes resource exists and has correct metadata
        routes_resource = await assert_resource_exists(session, "apx://routes")
        assert routes_resource.name == "api-routes"
        assert routes_resource.description is not None
        assert "OpenAPI" in routes_resource.description
        assert routes_resource.mimeType == "application/json"

        # Read and validate routes
        content = await session.read_resource(AnyUrl("apx://routes"))
        assert len(content.contents) == 1
        contents = content.contents[0]
        assert isinstance(contents, TextResourceContents)

        routes_json = json.loads(contents.text)
        assert isinstance(routes_json, list)
        assert len(routes_json) == 6

        # Verify all expected routes exist
        route_ids = [r["id"] for r in routes_json]
        expected_routes = [
            "listItems",
            "getItem",
            "createItem",
            "updateItem",
            "partialUpdateItem",
            "deleteItem",
        ]
        for route_id in expected_routes:
            assert route_id in route_ids, f"Should have {route_id} route"

        # Verify route structure
        list_items_route = next(r for r in routes_json if r["id"] == "listItems")
        assert list_items_route["method"] == "GET"
        assert "/items" in list_items_route["path"]

        create_item_route = next(r for r in routes_json if r["id"] == "createItem")
        assert create_item_route["method"] == "POST"
        assert "/items" in create_item_route["path"]

        # Verify get_route_info tool exists
        await assert_tool_exists(session, "get_route_info")

        # Test GET operation (query hook)
        result = await session.call_tool(
            "get_route_info", arguments={"operation_id": "listItems"}
        )
        result_json = parse_json_result(result)
        assert result_json["operation_id"] == "listItems"
        assert result_json["method"] == "GET"
        _assert_query_example(result_json["example"], "ListItems")

        # Test POST operation (mutation hook)
        result = await session.call_tool(
            "get_route_info", arguments={"operation_id": "createItem"}
        )
        result_json = parse_json_result(result)
        assert result_json["operation_id"] == "createItem"
        assert result_json["method"] == "POST"
        _assert_mutation_example(result_json["example"], "CreateItem")

        # Test non-existent operation ID
        result = await session.call_tool(
            "get_route_info", arguments={"operation_id": "nonExistentOperation"}
        )
        result_text = retrieve_text_content(result)
        assert "not found" in result_text.lower()
