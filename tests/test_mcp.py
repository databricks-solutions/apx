"""Tests for the Rust MCP server implementation."""

import json
import os
from contextlib import asynccontextmanager
from pathlib import Path
from importlib import resources

from mcp.types import TextResourceContents
from pydantic import AnyUrl
import pytest
from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client

from apx._core import run_cli


@pytest.fixture
def apx_source_dir():
    """Get the apx source directory for editable installs."""
    return str(Path(str(resources.files("apx"))).parent.parent)


@pytest.fixture
def init_test_project(tmp_path, apx_source_dir):
    """Initialize a minimal test project without dependencies."""
    exit_code = run_cli(
        [
            "apx",
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
        ]
    )
    assert exit_code == 0, "Failed to initialize test project"
    assert (tmp_path / "src").exists(), "src directory should exist"
    return tmp_path


@asynccontextmanager
async def mcp_session(project_dir):
    """Context manager for MCP client session."""
    original_cwd = os.getcwd()
    try:
        os.chdir(project_dir)
        server_params = StdioServerParameters(
            command="uv", args=["run", "--no-sync", "apx", "mcp"]
        )
        async with stdio_client(server_params) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                yield session
    finally:
        os.chdir(original_cwd)


def parse_json_result(result):
    """Parse JSON from MCP tool result."""
    assert len(result.content) > 0, "Result should have content"
    result_text = result.content[0].text
    assert result_text.startswith("{"), f"Expected JSON response, got: {result_text}"
    return json.loads(result_text)


@pytest.mark.asyncio
async def test_apx_info_resource():
    """Test that the Rust MCP server provides the apx://info resource."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # List resources
            resources = await session.list_resources()
            uris = [str(r.uri) for r in resources.resources]
            assert "apx://info" in uris

            # Find the apx://info resource
            apx_info_resource = next(
                r for r in resources.resources if str(r.uri) == "apx://info"
            )
            assert apx_info_resource.name == "apx-info"
            desc = apx_info_resource.description
            assert desc is not None
            assert "apx toolkit" in desc.lower()
            assert apx_info_resource.mimeType == "text/plain"

            # Read the resource
            content = await session.read_resource(AnyUrl("apx://info"))
            assert len(content.contents) == 1
            contents = content.contents[0]
            assert isinstance(contents, TextResourceContents)
            text = contents.text
            assert text is not None
            assert "apx" in text.lower()
            assert "Databricks Apps" in text
            assert "Technology Stack" in text


@pytest.mark.asyncio
async def test_start_tool_exists():
    """Test that the start tool is available."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # List tools
            tools = await session.list_tools()
            tool_names = [t.name for t in tools.tools]
            assert "start" in tool_names

            # Find the start tool
            start_tool = next(t for t in tools.tools if t.name == "start")
            desc = start_tool.description
            assert desc is not None
            assert "start development server" in desc.lower()
            assert "inputSchema" in dir(start_tool)


@pytest.mark.asyncio
async def test_mcp_server_capabilities():
    """Test that the MCP server advertises correct capabilities."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

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


@pytest.mark.asyncio
async def test_search_and_add_component(init_test_project):
    """Test search_registry_components and add_component tools."""
    tmp_path = init_test_project

    async with mcp_session(tmp_path) as session:
        # Test 1: List tools and verify search_registry_components exists
        tools = await session.list_tools()
        tool_names = [t.name for t in tools.tools]
        assert "search_registry_components" in tool_names, (
            "search_registry_components tool should exist"
        )
        assert "add_component" in tool_names, "add_component tool should exist"

        # Test 2: Search for button component
        search_result = await session.call_tool(
            "search_registry_components",
            arguments={"query": "button component for clicking", "limit": 5},
        )

        result_json = parse_json_result(search_result)
        print(
            f"\n=== Search Result ===\n{json.dumps(result_json, indent=2)}\n====================\n"
        )

        assert "query" in result_json, "Result should contain query"
        assert "results" in result_json, "Result should contain results"
        assert result_json["query"] == "button component for clicking"
        results = result_json["results"]
        print(f"✓ Search returned {len(results)} results")

        # Verify "button" is in the results (should have high similarity)
        result_ids = [r["id"] for r in results]
        assert "button" in result_ids, (
            "Button should be in search results for button query"
        )

        # Check that button has the highest score
        button_result = next(r for r in results if r["id"] == "button")
        assert button_result["score"] > 0.85, "Button should have high similarity score"

        # Test 3: Add a component
        add_result = await session.call_tool(
            "add_component", arguments={"component_id": "dialog", "force": False}
        )

        assert len(add_result.content) > 0, "Add should return content"
        result_text = add_result.content[0].text
        print(f"\n=== Add Component Result ===\n{result_text}\n====================\n")

        # Verify that if successful, the dialog file was created
        dialog_file = (
            tmp_path / "src" / "test_app" / "ui" / "components" / "ui" / "dialog.tsx"
        )
        if "Successfully added component" in result_text:
            assert dialog_file.exists(), "Dialog component file should be created"
            content = dialog_file.read_text()
            assert len(content) > 0, "Dialog component should have content"
            assert "dialog" in content.lower(), (
                "Dialog component should contain 'dialog'"
            )
            print(f"✓ Dialog component created successfully at {dialog_file}")
        else:
            print(f"Component add returned: {result_text}")


@pytest.mark.asyncio
async def test_search_and_add_custom_registry_component(init_test_project):
    """Test search and add for custom registry components (e.g., @animate-ui)."""
    tmp_path = init_test_project

    async with mcp_session(tmp_path) as session:
        # Test 1: Search for sidebar component from animate-ui
        search_result = await session.call_tool(
            "search_registry_components",
            arguments={"query": "animated sidebar navigation component", "limit": 5},
        )

        result_json = parse_json_result(search_result)
        print(
            f"\n=== Search for Custom Registry Component ===\n{json.dumps(result_json, indent=2)}\n====================\n"
        )

        assert "query" in result_json
        assert "results" in result_json
        results = result_json["results"]
        print(f"✓ Search returned {len(results)} results")

        # Check if any results are from @animate-ui registry
        animate_ui_results = [
            r for r in results if r.get("id", "").startswith("@animate-ui")
        ]
        if animate_ui_results:
            print(
                f"  Found {len(animate_ui_results)} results from @animate-ui registry"
            )

        # Verify we got results from default registry
        default_results = [r for r in results if not r.get("id", "").startswith("@")]
        assert len(default_results) > 0, "Should have results from default registry"

        # Test 2: Add a custom registry component - @animate-ui/components-radix-sidebar
        print(
            "\n=== Adding Custom Registry Component (@animate-ui/components-radix-sidebar) ==="
        )
        add_result = await session.call_tool(
            "add_component",
            arguments={
                "component_id": "@animate-ui/components-radix-sidebar",
                "force": False,
            },
        )

        assert len(add_result.content) > 0, "Add should return content"
        result_text = add_result.content[0].text
        print(f"\n{result_text}\n====================\n")

        # Verify that if successful, the sidebar file was created
        sidebar_file = (
            tmp_path
            / "src"
            / "test_app"
            / "ui"
            / "components"
            / "animate-ui"
            / "components-radix-sidebar.tsx"
        )

        if "Successfully added component" in result_text:
            assert sidebar_file.exists(), (
                f"Sidebar component should be created at {sidebar_file}"
            )
            content = sidebar_file.read_text()
            assert len(content) > 0, "Sidebar component should have content"
            print(f"✓ Custom registry component created successfully at {sidebar_file}")

            content_lower = content.lower()
            assert any(
                term in content_lower for term in ["sidebar", "navigation", "nav"]
            ), "Component should contain sidebar/navigation related content"
        elif "Failed to add component" in result_text:
            print(f"⚠ Component add failed (acceptable for test):\n  {result_text}")
            assert any(
                term in result_text
                for term in [
                    "Failed to fetch",
                    "Registry returned error",
                    "Unknown registry",
                    "404",
                    "File already exists",
                    "Failed to",
                ]
            ), "Should have a clear error message"
        else:
            print(f"Component add result: {result_text}")


@pytest.mark.asyncio
async def test_docs_tool():
    """Test the docs tool for searching Databricks SDK documentation."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # Test 1: List tools and verify docs tool exists
            tools = await session.list_tools()
            tool_names = [t.name for t in tools.tools]
            assert "docs" in tool_names, "docs tool should exist"

            # Test 2: Search for cluster-related docs
            print("\n=== Testing docs tool: searching for cluster documentation ===")
            search_result = await session.call_tool(
                "docs",
                arguments={
                    "source": "databricks-sdk-python",
                    "query": "create cluster",
                    "num_results": 3,
                },
            )

            assert len(search_result.content) > 0, "Search should return content"
            result_text = search_result.content[0].text

            # Check if we got an error or actual results
            if "not available" in result_text or "not installed" in result_text:
                print(f"⚠ SDK docs not available (acceptable for test): {result_text}")
                pytest.skip("Databricks SDK not installed or docs not indexed")
            else:
                # We got results, parse and validate
                result_json = json.loads(result_text)
                print(
                    f"\n=== Docs Search Result ===\n{json.dumps(result_json, indent=2)}\n====================\n"
                )

                assert "source" in result_json
                assert "query" in result_json
                assert "results" in result_json
                assert result_json["source"] == "databricks-sdk-python"
                assert result_json["query"] == "create cluster"

                results = result_json["results"]
                assert len(results) <= 3, "Should respect num_results limit"

                if len(results) > 0:
                    # Validate structure of first result
                    first_result = results[0]
                    assert "text" in first_result
                    assert "source_file" in first_result
                    assert "score" in first_result

                    # Text should be non-empty
                    assert len(first_result["text"]) > 0

                    # Source file should be a workspace RST file
                    assert "workspace" in first_result["source_file"]

                    # Score should be between 0 and 1
                    assert 0 <= first_result["score"] <= 1

                    print(f"✓ Found {len(results)} relevant documentation chunks")
                    print(f"  Top result score: {first_result['score']:.3f}")
                    print(f"  Top result file: {first_result['source_file']}")
                else:
                    print("⚠ No results found (may be normal for specific queries)")


@pytest.mark.asyncio
async def test_docs_create_cluster():
    """Test that 'create cluster' query returns relevant cluster creation docs in top 3."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # Search for "create cluster" with 5 results
            result = await session.call_tool(
                "docs",
                arguments={
                    "source": "databricks-sdk-python",
                    "query": "create cluster",
                    "num_results": 5,
                },
            )

            assert len(result.content) > 0, "Search should return content"
            result_text = result.content[0].text

            # Debug: Print the actual response
            print(f"\n=== Raw Response ===")
            print(f"Response text: {result_text[:500]}...")
            print(f"Response length: {len(result_text)}")
            print(f"===================\n")

            # Skip if SDK not available
            if "not available" in result_text or "not installed" in result_text:
                pytest.skip("Databricks SDK not installed or docs not indexed")

            # Parse results
            try:
                result_json = json.loads(result_text)
            except json.JSONDecodeError as e:
                print(f"\n=== JSON Parse Error ===")
                print(f"Error: {e}")
                print(f"Full response text:\n{result_text}")
                print(f"=======================\n")
                raise
            results = result_json["results"]

            print(f"\n=== Test: 'create cluster' relevance ===")
            print(f"Query: {result_json['query']}")
            print(f"Total results: {len(results)}")

            # At least one of top 3 should be cluster-related
            top_3 = results[:3]
            cluster_related = [
                r
                for r in top_3
                if "cluster" in r["source_file"].lower()
                or "cluster" in r["text"].lower()
            ]

            print(f"\nTop 3 results:")
            for i, r in enumerate(top_3, 1):
                print(f"  {i}. Score: {r['score']:.3f}, File: {r['source_file']}")
                is_cluster = "✓ cluster-related" if r in cluster_related else ""
                if is_cluster:
                    print(f"     {is_cluster}")

            assert len(cluster_related) >= 1, (
                f"Expected at least 1 cluster-related result in top 3, got {len(cluster_related)}. "
                f"Top 3 files: {[r['source_file'] for r in top_3]}"
            )

            print(
                f"\n✓ Test passed: {len(cluster_related)} cluster-related result(s) in top 3"
            )


@pytest.mark.asyncio
async def test_routes_resource_and_get_route_info(e2e_init):
    """Test routes resource and get_route_info tool with generated OpenAPI."""
    tmp_path = e2e_init
    src_dir = tmp_path / "src"
    backend_dir = src_dir / "test_app" / "backend"

    # Create a backend router with rich REST operations
    router_code = """from fastapi import APIRouter, HTTPException
from pydantic import BaseModel
from .config import conf

api = APIRouter(prefix=conf.api_prefix)

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

    # Write the router code
    (backend_dir / "router.py").write_text(router_code)

    # Create the _version.py file that would normally be generated by hatch
    (src_dir / "test_app" / "_version.py").write_text('version = "0.0.0"\n')

    # Run the __generate_openapi CLI command
    exit_code = run_cli(
        [
            "apx",
            "__generate_openapi",
            "--app-dir",
            str(tmp_path),
            "--force",
        ]
    )

    assert exit_code == 0, "OpenAPI generation should succeed"

    # Verify OpenAPI file was created
    openapi_path = tmp_path / ".apx" / "openapi.json"
    assert openapi_path.exists(), "OpenAPI schema should be generated"

    # Print the generated OpenAPI schema
    openapi_schema = json.loads(openapi_path.read_text())
    print(
        f"\n=== Generated OpenAPI Schema ===\n{json.dumps(openapi_schema, indent=2)}\n====================\n"
    )

    # print the generated api.ts file
    api_ts_path = tmp_path / "src" / "test_app" / "ui" / "lib" / "api.ts"
    print(
        f"\n=== Generated api.ts file ===\n{api_ts_path.read_text()}\n====================\n"
    )

    # Test the MCP server with the generated routes
    async with mcp_session(tmp_path) as session:
        # Test 1: List resources and verify routes resource exists
        resources = await session.list_resources()
        uris = [str(r.uri) for r in resources.resources]
        assert "apx://routes" in uris, "routes resource should exist"

        # Find the apx://routes resource
        routes_resource = next(
            r for r in resources.resources if str(r.uri) == "apx://routes"
        )
        assert routes_resource.name == "api-routes"
        assert "OpenAPI" in routes_resource.description
        assert routes_resource.mimeType == "application/json"

        # Test 2: Read the routes resource
        print("\n=== Reading routes resource ===")
        content = await session.read_resource(AnyUrl("apx://routes"))
        assert len(content.contents) == 1
        contents = content.contents[0]
        assert isinstance(contents, TextResourceContents)

        routes_json = json.loads(contents.text)
        print(
            f"\n=== Routes ===\n{json.dumps(routes_json, indent=2)}\n====================\n"
        )

        # Verify we got the expected routes
        assert isinstance(routes_json, list), "Routes should be a list"
        assert len(routes_json) == 6, f"Should have 6 routes, got {len(routes_json)}"

        # Check specific routes
        route_ids = [r["id"] for r in routes_json]
        assert "listItems" in route_ids, "Should have listItems route"
        assert "getItem" in route_ids, "Should have getItem route"
        assert "createItem" in route_ids, "Should have createItem route"
        assert "updateItem" in route_ids, "Should have updateItem route"
        assert "partialUpdateItem" in route_ids, "Should have partialUpdateItem route"
        assert "deleteItem" in route_ids, "Should have deleteItem route"

        # Verify route structure
        list_items_route = next(r for r in routes_json if r["id"] == "listItems")
        assert list_items_route["method"] == "GET"
        assert "/items" in list_items_route["path"]
        assert "List Items" in list_items_route["description"]

        create_item_route = next(r for r in routes_json if r["id"] == "createItem")
        assert create_item_route["method"] == "POST"
        assert "/items" in create_item_route["path"]

        print(f"✓ Routes resource validated: {len(routes_json)} routes found")

        # Test 3: Verify get_route_info tool exists
        tools = await session.list_tools()
        tool_names = [t.name for t in tools.tools]
        assert "get_route_info" in tool_names, "get_route_info tool should exist"

        # Test 4: Get route info for a GET operation (query)
        print("\n=== Testing get_route_info for GET operation (listItems) ===")
        result = await session.call_tool(
            "get_route_info",
            arguments={"operation_id": "listItems"},
        )

        result_json = parse_json_result(result)
        print(
            f"\n=== listItems Route Info ===\n{json.dumps(result_json, indent=2)}\n====================\n"
        )

        assert result_json["operation_id"] == "listItems"
        assert result_json["method"] == "GET"
        assert "example" in result_json

        # Verify the example contains expected patterns for a query
        example = result_json["example"]
        # Standard query hook
        assert 'import { useListItems } from "@/lib/api"' in example
        assert 'import selector from "@/lib/selector"' in example
        assert "const { data, isLoading, error } = useListItems(selector())" in example
        assert "if (isLoading)" in example
        assert "if (error)" in example
        # Suspense query hook
        assert 'import { useListItemsSuspense } from "@/lib/api"' in example
        assert "const { data } = useListItemsSuspense(selector())" in example
        assert "Suspense boundary" in example
        # Type hints
        assert "ListItemsQueryResult" in example
        assert "ListItemsQueryError" in example

        print(
            f"✓ Query example validated (includes standard/suspense hooks and type hints)"
        )
        print(f"Example code:\n{example}\n")

        # Test 5: Get route info for a POST operation (mutation)
        print("\n=== Testing get_route_info for POST operation (createItem) ===")
        result = await session.call_tool(
            "get_route_info",
            arguments={"operation_id": "createItem"},
        )

        result_json = parse_json_result(result)
        print(
            f"\n=== createItem Route Info ===\n{json.dumps(result_json, indent=2)}\n====================\n"
        )

        assert result_json["operation_id"] == "createItem"
        assert result_json["method"] == "POST"
        assert "example" in result_json

        # Verify the example contains expected patterns for a mutation
        example = result_json["example"]
        assert 'import { useCreateItem } from "@/lib/api"' in example
        assert "const { mutate, isPending } = useCreateItem()" in example
        assert "mutate({ data: { /* request body */ } })" in example
        assert "onClick={handleSubmit}" in example
        # Type hints
        assert "CreateItemMutationBody" in example
        assert "CreateItemMutationResult" in example
        assert "CreateItemMutationError" in example

        print(f"✓ Mutation example validated (includes type hints)")
        print(f"Example code:\n{example}\n")

        # Test 6: Test with non-existent operation ID
        print("\n=== Testing get_route_info with invalid operation ID ===")
        result = await session.call_tool(
            "get_route_info",
            arguments={"operation_id": "nonExistentOperation"},
        )

        assert len(result.content) > 0
        result_text = result.content[0].text
        assert "not found" in result_text.lower(), (
            "Should return error for invalid operation ID"
        )
        print(f"✓ Error handling validated: {result_text}")

        print("\n✓ All routes resource and get_route_info tests passed!")
