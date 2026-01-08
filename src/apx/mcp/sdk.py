"""Databricks SDK documentation MCP tools.

Provides tools for searching and retrieving documentation from the Databricks SDK.
Uses an in-memory SQLite FTS5 index for efficient full-text search with SQLModel
for typed database operations.
"""

import inspect
import io
import zipfile
from dataclasses import fields, is_dataclass
from pathlib import Path

import httpx
from sqlalchemy import text
from sqlmodel import Session, SQLModel, col, create_engine, select

from apx.mcp.models import (
    SDKMethodSpec,
    SDKMethodSpecResponse,
    SDKMethodTable,
    SDKModelField,
    SDKModelSpec,
    SDKModelSpecResponse,
    SDKModelTable,
    SDKParameterInfo,
    SDKSearchResult,
    SDKUsageInstructions,
)
from apx.mcp.sdk_parser import enrich_method_with_rst, load_all_rst_docs
from apx.mcp.server import mcp

# Module-level singleton for the SDK index
_engine = None
_index_stats: dict[str, int | float] = {"methods": 0, "models": 0, "rst_coverage": 0.0}

# Cache configuration
CACHE_DIR = Path.home() / ".apx" / "caches" / "sdk"
GITHUB_REPO = "databricks/databricks-sdk-py"

CUSTOM_USAGE_INSTRUCTIONS = """
# Custom Usage Instructions

- prefer using `get_obo_ws` dependency and OBO-based workspace client if provided
- the Databricks SDK is synchronous, so use asyncio.to_thread if you need to run asynchronous code
- Prefer not to use the RestAPI directly, use the SDK instead, only use RestAPI if explicitly requesteds
"""


# ============================================================================
# SDK GitHub Repository Download and Caching
# ============================================================================


def get_installed_sdk_version() -> str | None:
    """Get the installed databricks-sdk version."""
    try:
        from importlib.metadata import version

        return version("databricks-sdk")
    except Exception:
        return None


def get_github_zipball_url(version: str) -> str:
    """Get the GitHub zipball URL for a specific version."""
    return f"https://github.com/{GITHUB_REPO}/archive/refs/tags/v{version}.zip"


def get_cache_path(version: str) -> Path:
    """Get the cache directory path for a specific SDK version."""
    return CACHE_DIR / version


def is_cached(version: str) -> bool:
    """Check if the SDK docs are already cached for this version."""
    cache_path = get_cache_path(version)
    # Check if docs folder exists with some content
    docs_path = cache_path / "docs"
    return docs_path.exists() and any(docs_path.iterdir())


def download_and_extract_sdk(version: str, force: bool = False) -> Path:
    """Download and extract the SDK repository for a specific version.

    Returns the path to the extracted docs folder.

    Args:
        version: The SDK version to download (e.g., "0.77.0")
        force: If True, re-download even if cached

    Returns:
        Path to the extracted docs folder
    """
    cache_path = get_cache_path(version)
    docs_path = cache_path / "docs"

    if not force and is_cached(version):
        return docs_path

    # Download the zipball
    url = get_github_zipball_url(version)

    response = httpx.get(url, follow_redirects=True, timeout=60.0)
    response.raise_for_status()

    # Create cache directory
    cache_path.mkdir(parents=True, exist_ok=True)

    # Extract on-the-fly
    with zipfile.ZipFile(io.BytesIO(response.content)) as zf:
        # The zip contains a root folder like "databricks-sdk-py-0.77.0/"
        # We need to find it and extract specific folders
        root_folder = None
        for name in zf.namelist():
            parts = name.split("/")
            if len(parts) > 1 and root_folder is None:
                root_folder = parts[0]
            break

        # Extract docs folder and the SDK source
        for member in zf.namelist():
            if not member.startswith(f"{root_folder}/"):
                continue

            relative_path = member[len(f"{root_folder}/") :]

            # We want: docs/, src/databricks/sdk/
            should_extract = relative_path.startswith(
                "docs/"
            ) or relative_path.startswith("src/databricks/sdk/")

            if should_extract:
                # Extract to cache path
                target_path = cache_path / relative_path
                if member.endswith("/"):
                    target_path.mkdir(parents=True, exist_ok=True)
                else:
                    target_path.parent.mkdir(parents=True, exist_ok=True)
                    with zf.open(member) as src:
                        target_path.write_bytes(src.read())

    return docs_path


def initialize_sdk_cache() -> dict[str, str | bool]:
    """Initialize SDK cache by downloading the repository for the current version.

    This is called during MCP server startup. If download fails, it's silently
    skipped as the cache is optional.

    Returns:
        Dictionary with status information about the cache initialization
    """
    try:
        version = get_installed_sdk_version()
        if version is None:
            return {
                "status": "skipped",
                "reason": "databricks-sdk not installed",
                "cached": False,
            }

        if is_cached(version):
            return {
                "status": "cached",
                "version": version,
                "path": str(get_cache_path(version)),
                "cached": True,
            }

        # Download in the background
        docs_path = download_and_extract_sdk(version)
        return {
            "status": "downloaded",
            "version": version,
            "path": str(docs_path),
            "cached": True,
        }
    except Exception as e:
        # Silently skip on any error - cache is optional
        return {
            "status": "failed",
            "reason": str(e),
            "cached": False,
        }


# ============================================================================
# SDK Introspection
# ============================================================================


def _extract_methods_from_sdk() -> list[SDKMethodSpec]:
    """Extract all API methods from the Databricks SDK using introspection."""
    methods: list[SDKMethodSpec] = []

    try:
        from databricks.sdk import WorkspaceClient
    except ImportError:
        return methods

    # Get all service attributes from WorkspaceClient
    for attr_name in dir(WorkspaceClient):
        if attr_name.startswith("_"):
            continue

        try:
            attr = getattr(WorkspaceClient, attr_name)

            # Check if it's a property that returns an API class
            if not isinstance(attr, property) or not attr.fget:
                continue

            # Get the return type annotation
            annotations = getattr(attr.fget, "__annotations__", {})
            return_type = annotations.get("return")

            if return_type is None:
                continue

            # Get the actual class
            api_class = return_type if isinstance(return_type, type) else None
            if api_class is None:
                continue

            # Extract methods from the API class
            for method_name in dir(api_class):
                if method_name.startswith("_"):
                    continue

                method = getattr(api_class, method_name)
                if not callable(method):
                    continue

                # Get method signature and docstring
                sig: inspect.Signature | None = None
                try:
                    sig = inspect.signature(method)
                    sig_str = str(sig)
                except (ValueError, TypeError):
                    sig_str = "(unknown)"

                docstring = method.__doc__ or ""

                # Extract parameters
                parameters: list[SDKParameterInfo] = []
                if sig is not None:
                    try:
                        for param_name, param in sig.parameters.items():
                            if param_name == "self":
                                continue
                            param_info = SDKParameterInfo(
                                name=param_name,
                                kind=str(param.kind.name),
                                default=(
                                    str(param.default)
                                    if param.default != inspect.Parameter.empty
                                    else None
                                ),
                                annotation=(
                                    str(param.annotation)
                                    if param.annotation != inspect.Parameter.empty
                                    else None
                                ),
                            )
                            parameters.append(param_info)
                    except Exception:
                        pass

                methods.append(
                    SDKMethodSpec(
                        service_name=attr_name,
                        class_name=api_class.__name__,
                        method_name=method_name,
                        full_name=f"{attr_name}.{method_name}",
                        signature=f"{method_name}{sig_str}",
                        docstring=docstring,
                        parameters=parameters,
                    )
                )

        except Exception:
            # Skip problematic attributes
            continue

    return methods


def _extract_models_from_sdk() -> list[SDKModelSpec]:
    """Extract all dataclasses/models from the Databricks SDK."""
    models: list[SDKModelSpec] = []

    try:
        from databricks.sdk import service
    except ImportError:
        return models

    # Get all submodules from the service package
    for module_name in dir(service):
        if module_name.startswith("_"):
            continue

        try:
            module = getattr(service, module_name)
            if not hasattr(module, "__file__"):
                continue

            # Find all dataclasses in the module
            for class_name in dir(module):
                if class_name.startswith("_"):
                    continue

                obj = getattr(module, class_name)
                if not isinstance(obj, type) or not is_dataclass(obj):
                    continue

                # Extract fields
                model_fields: list[SDKModelField] = []
                try:
                    for field in fields(obj):
                        model_fields.append(
                            SDKModelField(
                                name=field.name,
                                type_annotation=str(field.type),
                                default=(
                                    str(field.default)
                                    if field.default is not None
                                    and str(field.default) != "MISSING"
                                    else None
                                ),
                            )
                        )
                except Exception:
                    pass

                models.append(
                    SDKModelSpec(
                        module_name=module_name,
                        class_name=class_name,
                        full_name=f"{module_name}.{class_name}",
                        docstring=obj.__doc__ or "",
                        fields=model_fields,
                    )
                )

        except Exception:
            continue

    return models


def _get_engine():
    """Get or create the SQLModel engine with FTS5 tables.

    Lazy-initializes the in-memory SQLite database on first call.
    """
    global _engine, _index_stats

    if _engine is not None:
        return _engine

    # Create in-memory SQLite database with SQLModel
    engine = create_engine("sqlite:///:memory:", echo=False)

    # Create SQLModel tables
    SQLModel.metadata.create_all(engine)

    # Create FTS5 virtual tables (SQLModel doesn't support these natively)
    with engine.connect() as conn:
        # FTS5 for methods
        conn.execute(
            text("""
            CREATE VIRTUAL TABLE IF NOT EXISTS sdk_methods_fts USING fts5(
                service_name,
                class_name,
                method_name,
                full_name,
                signature,
                docstring,
                rst_docs,
                content='',
                tokenize='porter unicode61'
            )
        """)
        )

        # FTS5 for models
        conn.execute(
            text("""
            CREATE VIRTUAL TABLE IF NOT EXISTS sdk_models_fts USING fts5(
                module_name,
                class_name,
                full_name,
                docstring,
                field_names,
                content='',
                tokenize='porter unicode61'
            )
        """)
        )
        conn.commit()

    # Populate the database
    _populate_index(engine)

    _engine = engine
    return engine


def _populate_index(engine) -> None:
    """Populate the database with SDK methods and models."""
    global _index_stats

    # Extract data from SDK
    methods = _extract_methods_from_sdk()
    models = _extract_models_from_sdk()

    # Try to load RST documentation
    rst_docs = {}
    try:
        version = get_installed_sdk_version()
        if version and is_cached(version):
            docs_path = get_cache_path(version) / "docs"
            rst_docs = load_all_rst_docs(docs_path)
    except Exception:
        # If RST loading fails, continue without it
        pass

    # Enrich methods with RST documentation
    enriched_methods = []
    rst_count = 0
    for method in methods:
        enriched = enrich_method_with_rst(method, rst_docs)
        enriched_methods.append(enriched)
        if enriched.has_rst:
            rst_count += 1

    with Session(engine) as session:
        # Insert methods
        for i, spec in enumerate(enriched_methods):
            row = SDKMethodTable.from_spec(spec, row_id=i + 1)
            session.add(row)

        session.commit()

        # Insert into FTS5 table for methods
        for i, spec in enumerate(enriched_methods):
            session.execute(
                text("""
                INSERT INTO sdk_methods_fts(rowid, service_name, class_name, method_name,
                                            full_name, signature, docstring, rst_docs)
                VALUES (:rowid, :service_name, :class_name, :method_name,
                        :full_name, :signature, :docstring, :rst_docs)
            """),
                {
                    "rowid": i + 1,
                    "service_name": spec.service_name,
                    "class_name": spec.class_name,
                    "method_name": spec.method_name,
                    "full_name": spec.full_name,
                    "signature": spec.signature,
                    "docstring": spec.docstring or "",
                    "rst_docs": spec.rst_docs or "",
                },
            )

        # Insert models
        for j, model_spec in enumerate(models):
            model_row = SDKModelTable.from_spec(model_spec, row_id=j + 1)
            session.add(model_row)

        session.commit()

        # Insert into FTS5 table for models
        for j, model_spec in enumerate(models):
            field_names = " ".join(f.name for f in model_spec.fields)
            session.execute(
                text("""
                INSERT INTO sdk_models_fts(rowid, module_name, class_name, full_name,
                                           docstring, field_names)
                VALUES (:rowid, :module_name, :class_name, :full_name,
                        :docstring, :field_names)
            """),
                {
                    "rowid": j + 1,
                    "module_name": model_spec.module_name,
                    "class_name": model_spec.class_name,
                    "full_name": model_spec.full_name,
                    "docstring": model_spec.docstring or "",
                    "field_names": field_names,
                },
            )

        session.commit()

    _index_stats["methods"] = len(enriched_methods)
    _index_stats["models"] = len(models)
    _index_stats["rst_coverage"] = (
        (rst_count / len(enriched_methods) * 100) if enriched_methods else 0.0
    )


def _search_methods(query: str, limit: int = 10) -> list[SDKMethodSpec]:
    """Search for methods using FTS5 full-text search."""
    engine = _get_engine()

    # Prepare FTS5 query - use OR between words for broader matching
    words = query.strip().split()
    if not words:
        return []

    fts_query = " OR ".join(words)

    with Session(engine) as session:
        # FTS5 search with join to get full data from SQLModel table
        result = session.execute(
            text("""
            SELECT m.id
            FROM sdk_methods_fts fts
            JOIN sdk_methods m ON fts.rowid = m.id
            WHERE sdk_methods_fts MATCH :query
            ORDER BY bm25(sdk_methods_fts)
            LIMIT :limit
        """),
            {"query": fts_query, "limit": limit},
        )

        # Get the IDs and fetch full rows using SQLModel
        ids = [row[0] for row in result]

        if not ids:
            return []

        # Fetch full rows using SQLModel (typed!)
        methods = session.exec(
            select(SDKMethodTable).where(col(SDKMethodTable.id).in_(ids))
        ).all()

        # Preserve FTS ranking order
        id_order = {id_: i for i, id_ in enumerate(ids)}
        methods = sorted(methods, key=lambda m: id_order.get(m.id or 0, 999))

        return [m.to_spec() for m in methods]


def _search_models(query: str, limit: int = 10) -> list[SDKModelSpec]:
    """Search for models using FTS5 full-text search."""
    engine = _get_engine()

    # Prepare FTS5 query
    words = query.strip().split()
    if not words:
        return []

    fts_query = " OR ".join(words)

    with Session(engine) as session:
        # FTS5 search with join
        result = session.execute(
            text("""
            SELECT m.id
            FROM sdk_models_fts fts
            JOIN sdk_models m ON fts.rowid = m.id
            WHERE sdk_models_fts MATCH :query
            ORDER BY bm25(sdk_models_fts)
            LIMIT :limit
        """),
            {"query": fts_query, "limit": limit},
        )

        ids = [row[0] for row in result]

        if not ids:
            return []

        # Fetch full rows using SQLModel
        models = session.exec(
            select(SDKModelTable).where(col(SDKModelTable.id).in_(ids))
        ).all()

        # Preserve FTS ranking order
        id_order = {id_: i for i, id_ in enumerate(ids)}
        models = sorted(models, key=lambda m: id_order.get(m.id or 0, 999))

        return [m.to_spec() for m in models]


def _get_method_by_name(full_name: str) -> SDKMethodSpec | None:
    """Get a method specification by its full name using SQLModel."""
    engine = _get_engine()

    with Session(engine) as session:
        statement = select(SDKMethodTable).where(SDKMethodTable.full_name == full_name)
        method = session.exec(statement).first()

        if method:
            return method.to_spec()

    return None


def _get_model_by_name(full_name: str) -> SDKModelSpec | None:
    """Get a model specification by its full name using SQLModel."""
    engine = _get_engine()

    with Session(engine) as session:
        statement = select(SDKModelTable).where(SDKModelTable.full_name == full_name)
        model = session.exec(statement).first()

        if model:
            return model.to_spec()

    return None


# ============================================================================
# MCP Tools
# ============================================================================


@mcp.tool()
async def search_databricks_sdk(
    query: str,
    limit: int = 10,
    include_models: bool = True,
) -> SDKSearchResult:
    """Search the Databricks SDK documentation for methods and models.

    Uses full-text search with porter stemming to find relevant SDK methods
    and data models based on natural language queries.

    Args:
        query: Natural language search query (e.g., "create cluster", "list jobs",
               "SQL warehouse permissions", "run notebook")
        limit: Maximum number of results to return for each category (default: 10)
        include_models: Whether to also search for matching data models (default: True)

    Returns:
        SDKSearchResult with matching methods and models, plus index statistics

    Examples:
        - "create cluster" -> ClustersAPI.create, ClusterSpec, etc.
        - "list jobs" -> JobsAPI.list, BaseJob, etc.
        - "permissions" -> Various permission-related methods
        - "SQL warehouse" -> WarehousesAPI methods
    """
    # Ensure index is initialized (lazy loading)
    _get_engine()

    methods = _search_methods(query, limit)

    models: list[SDKModelSpec] = []
    if include_models:
        models = _search_models(query, limit)

    return SDKSearchResult(
        methods=methods,
        models=models,
        total_methods_indexed=int(_index_stats["methods"]),
        total_models_indexed=int(_index_stats["models"]),
        rst_coverage=float(_index_stats.get("rst_coverage", 0.0)),
    )


@mcp.tool()
async def get_method_spec(full_name: str) -> SDKMethodSpecResponse:
    """Get the full specification for a Databricks SDK method.

    Retrieves detailed information about a specific SDK method including
    its complete signature, docstring, and all parameters with types.

    Args:
        full_name: The full method name in format "service.method"
                   (e.g., "clusters.create", "jobs.list", "warehouses.get")

    Returns:
        SDKMethodSpecResponse with the method specification or an error message

    Examples:
        - "clusters.create" -> Full spec for ClustersAPI.create()
        - "jobs.run_now" -> Full spec for JobsAPI.run_now()
        - "catalogs.list" -> Full spec for CatalogsAPI.list()
    """
    # Ensure index is initialized
    _get_engine()

    method = _get_method_by_name(full_name)

    if method is None:
        # Try to provide helpful suggestions
        parts = full_name.split(".")
        if len(parts) == 2:
            service, method_name = parts
            # Search for similar methods
            similar = _search_methods(f"{service} {method_name}", limit=3)
            if similar:
                suggestions = ", ".join(m.full_name for m in similar)
                return SDKMethodSpecResponse(
                    method=None,
                    error=f"Method '{full_name}' not found. Did you mean: {suggestions}?",
                )

        return SDKMethodSpecResponse(
            method=None,
            error=f"Method '{full_name}' not found. Use search_databricks_sdk to find available methods.",
        )

    return SDKMethodSpecResponse(method=method, error=None)


@mcp.tool()
async def get_model_spec(full_name: str) -> SDKModelSpecResponse:
    """Get the full specification for a Databricks SDK data model/dataclass.

    Retrieves detailed information about a specific SDK dataclass including
    its docstring and all fields with types.

    Args:
        full_name: The full model name in format "module.ClassName"
                   (e.g., "jobs.JobSettings", "compute.ClusterSpec", "catalog.TableInfo")

    Returns:
        SDKModelSpecResponse with the model specification or an error message

    Examples:
        - "jobs.JobSettings" -> Full spec for JobSettings dataclass
        - "compute.AutoScale" -> Full spec for AutoScale dataclass
        - "catalog.TableInfo" -> Full spec for TableInfo dataclass
    """
    # Ensure index is initialized
    _get_engine()

    model = _get_model_by_name(full_name)

    if model is None:
        # Try to provide helpful suggestions
        parts = full_name.split(".")
        if len(parts) == 2:
            module, class_name = parts
            # Search for similar models
            similar = _search_models(f"{module} {class_name}", limit=3)
            if similar:
                suggestions = ", ".join(m.full_name for m in similar)
                return SDKModelSpecResponse(
                    model=None,
                    error=f"Model '{full_name}' not found. Did you mean: {suggestions}?",
                )

        return SDKModelSpecResponse(
            model=None,
            error=f"Model '{full_name}' not found. Use search_databricks_sdk to find available models.",
        )

    return SDKModelSpecResponse(model=model, error=None)


def _load_usage_guide(guide_name: str) -> str:
    """Load a usage guide from the SDK docs cache.

    Args:
        guide_name: Name of the guide file (e.g., "pagination.md", "wait.md")

    Returns:
        Contents of the guide file, or a message if not available
    """
    try:
        version = get_installed_sdk_version()
        if version and is_cached(version):
            docs_path = get_cache_path(version) / "docs" / guide_name
            if docs_path.exists():
                return docs_path.read_text(encoding="utf-8")
    except Exception:
        pass

    return (
        f"Guide '{guide_name}' not available. Please ensure SDK cache is initialized."
    )


@mcp.tool()
async def get_sdk_usage_instructions() -> SDKUsageInstructions:
    """Get comprehensive usage instructions for the Databricks SDK.

    Returns detailed guides covering:
    - Pagination: How to handle paginated API responses with iterators
    - Long-running operations: How to wait for async operations to complete
    - Custom instructions: Additional usage patterns and best practices

    These guides include code examples and recommended patterns for common
    SDK usage scenarios.

    Returns:
        SDKUsageInstructions with pagination guide, long-running operations guide,
        and custom instructions

    Examples:
        Use this tool to learn about:
        - Iterating through paginated results from list APIs
        - Waiting for cluster creation or job completion
        - Common patterns and best practices
    """
    pagination_guide = _load_usage_guide("pagination.md")
    wait_guide = _load_usage_guide("wait.md")

    return SDKUsageInstructions(
        pagination_guide=pagination_guide,
        long_running_operations_guide=wait_guide,
        custom_instructions=CUSTOM_USAGE_INSTRUCTIONS,
    )
