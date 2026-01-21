"""Databricks Lakebase MCP tools for database instance management and SQL operations.

This module provides MCP (Model Context Protocol) tools for:
- Listing and describing Lakebase database instances
- Getting and setting instance capacity (autoscaling)
- Running SQL queries against Lakebase instances
- Schema introspection (tables and columns)

The tools follow the same patterns as Neon MCP for PostgreSQL operations,
adapted for Databricks Lakebase instances.

Note: SQL execution and schema introspection require the 'lakebase' extra:
    uv add apx[lakebase]
"""

import asyncio
from typing import Any

from databricks.sdk import WorkspaceClient
from databricks.sdk.errors import NotFound
from sqlalchemy import create_engine, event, text

from apx.mcp.server import mcp
from apx.models import (
    LakebaseCapacityInfo,
    LakebaseCapacityUpdateResponse,
    LakebaseColumnInfo,
    LakebaseInstanceInfo,
    LakebaseListInstancesResponse,
    LakebaseSqlResult,
    LakebaseTableInfo,
    LakebaseTableSchemaResponse,
    LakebaseTablesResponse,
    McpErrorResponse,
)

# Check if psycopg is available for PostgreSQL connectivity
def _check_psycopg_available() -> bool:
    try:
        import psycopg  # noqa: F401

        return True
    except ImportError:
        return False


_psycopg_available = _check_psycopg_available()

PSYCOPG_INSTALL_MSG = (
    "The psycopg PostgreSQL driver is not installed. "
    "Install the 'lakebase' extra to enable SQL functionality:\n"
    "  uv add apx[lakebase]"
)

# Available Lakebase capacity tiers
LAKEBASE_CAPACITIES = ["CU_1", "CU_2", "CU_4", "CU_8", "CU_16"]

# Default database name for Lakebase
DEFAULT_DATABASE_NAME = "databricks_postgres"

# Default port for Lakebase PostgreSQL
DEFAULT_PORT = 5432


def _get_workspace_client() -> WorkspaceClient:
    """Get a WorkspaceClient for Lakebase operations.

    Uses environment-based authentication (DATABRICKS_CONFIG_PROFILE or
    service principal credentials).
    """
    return WorkspaceClient()


def _get_lakebase_engine(
    ws: WorkspaceClient,
    instance_name: str,
    database_name: str = DEFAULT_DATABASE_NAME,
    port: int = DEFAULT_PORT,
) -> Any:
    """Create a SQLAlchemy engine for a Lakebase instance.

    Args:
        ws: WorkspaceClient for authentication
        instance_name: Name of the database instance
        database_name: Database name within the instance
        port: PostgreSQL port

    Returns:
        SQLAlchemy Engine configured for the Lakebase instance

    Raises:
        ImportError: If psycopg is not installed (requires apx[lakebase] extra)
    """
    if not _psycopg_available:
        raise ImportError(PSYCOPG_INSTALL_MSG)

    instance = ws.database.get_database_instance(instance_name)

    username = ws.config.client_id or ws.current_user.me().user_name
    host = instance.read_write_dns
    url = f"postgresql+psycopg://{username}:@{host}:{port}/{database_name}"

    engine = create_engine(
        url,
        connect_args={"sslmode": "require"},
    )

    # Set up dynamic password retrieval for token-based auth
    def before_connect(
        dialect: Any, conn_rec: Any, cargs: Any, cparams: dict[str, Any]
    ) -> None:
        cred = ws.database.generate_database_credential(instance_names=[instance_name])
        cparams["password"] = cred.token

    event.listen(engine, "do_connect", before_connect)

    return engine


# ============================================================================
# MCP Tools - Instance Management
# ============================================================================


@mcp.tool()
async def list_database_instances() -> LakebaseListInstancesResponse | McpErrorResponse:
    """List all Databricks Lakebase database instances.

    Returns a list of all database instances accessible to the current user,
    including their state, capacity, and endpoint information.

    Returns:
        LakebaseListInstancesResponse with all instances, or McpErrorResponse on error
    """
    try:
        ws = _get_workspace_client()

        def fetch_instances() -> list[LakebaseInstanceInfo]:
            instances = list(ws.database.list_database_instances())
            return [
                LakebaseInstanceInfo(
                    name=inst.name or "",
                    state=str(inst.state) if inst.state else "UNKNOWN",
                    capacity=str(inst.capacity) if inst.capacity else "UNKNOWN",
                    read_write_dns=inst.read_write_dns,
                    read_only_dns=inst.read_only_dns,
                    creator=inst.creator,
                    created_time=(
                        str(inst.creation_time) if inst.creation_time else None
                    ),
                )
                for inst in instances
            ]

        instances = await asyncio.to_thread(fetch_instances)
        return LakebaseListInstancesResponse(
            instances=instances, total_count=len(instances)
        )
    except Exception as e:
        return McpErrorResponse(error=f"Failed to list database instances: {e!s}")


@mcp.tool()
async def get_database_instance(
    instance_name: str,
) -> LakebaseInstanceInfo | McpErrorResponse:
    """Get detailed information about a specific Lakebase database instance.

    Args:
        instance_name: Name of the database instance to retrieve

    Returns:
        LakebaseInstanceInfo with instance details, or McpErrorResponse if not found
    """
    try:
        ws = _get_workspace_client()

        def fetch_instance() -> LakebaseInstanceInfo:
            inst = ws.database.get_database_instance(instance_name)
            return LakebaseInstanceInfo(
                name=inst.name or instance_name,
                state=str(inst.state) if inst.state else "UNKNOWN",
                capacity=str(inst.capacity) if inst.capacity else "UNKNOWN",
                read_write_dns=inst.read_write_dns,
                read_only_dns=inst.read_only_dns,
                creator=inst.creator,
                created_time=str(inst.creation_time) if inst.creation_time else None,
            )

        return await asyncio.to_thread(fetch_instance)
    except NotFound:
        return McpErrorResponse(
            error=f"Database instance '{instance_name}' not found. "
            "Use list_database_instances to see available instances."
        )
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get database instance: {e!s}")


# ============================================================================
# MCP Tools - Capacity/Autoscaling
# ============================================================================


@mcp.tool()
async def get_instance_capacity(
    instance_name: str,
) -> LakebaseCapacityInfo | McpErrorResponse:
    """Get current capacity and available scaling options for a Lakebase instance.

    Args:
        instance_name: Name of the database instance

    Returns:
        LakebaseCapacityInfo with current and available capacities
    """
    try:
        ws = _get_workspace_client()

        def fetch_capacity() -> LakebaseCapacityInfo:
            inst = ws.database.get_database_instance(instance_name)
            return LakebaseCapacityInfo(
                instance_name=instance_name,
                current_capacity=str(inst.capacity) if inst.capacity else "UNKNOWN",
                available_capacities=LAKEBASE_CAPACITIES,
            )

        return await asyncio.to_thread(fetch_capacity)
    except NotFound:
        return McpErrorResponse(error=f"Database instance '{instance_name}' not found.")
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get instance capacity: {e!s}")


@mcp.tool()
async def set_instance_capacity(
    instance_name: str,
    capacity: str,
    confirm: bool = False,
) -> LakebaseCapacityUpdateResponse | McpErrorResponse:
    """Set the capacity (scaling tier) for a Lakebase instance.

    This is a write operation that modifies the instance configuration.
    The capacity change may take a few minutes to complete.

    Available capacity tiers: CU_1, CU_2, CU_4, CU_8, CU_16

    Args:
        instance_name: Name of the database instance
        capacity: Target capacity tier (CU_1, CU_2, CU_4, CU_8, CU_16)
        confirm: Must be True to apply changes (safety check)

    Returns:
        LakebaseCapacityUpdateResponse with status, or McpErrorResponse on error
    """
    if capacity not in LAKEBASE_CAPACITIES:
        return McpErrorResponse(
            error=f"Invalid capacity '{capacity}'. "
            f"Must be one of: {', '.join(LAKEBASE_CAPACITIES)}"
        )

    if not confirm:
        return McpErrorResponse(
            error=f"Safety check: Set confirm=True to change capacity from current "
            f"value to '{capacity}'. This operation may affect running workloads."
        )

    try:
        ws = _get_workspace_client()

        def update_capacity() -> LakebaseCapacityUpdateResponse:
            from databricks.sdk.service.database import DatabaseInstance

            # Get current capacity first
            inst = ws.database.get_database_instance(instance_name)
            previous_capacity = str(inst.capacity) if inst.capacity else "UNKNOWN"

            # Update the instance capacity
            # Create a DatabaseInstance with the new capacity
            update_inst = DatabaseInstance(name=instance_name, capacity=capacity)
            ws.database.update_database_instance(
                name=instance_name,
                database_instance=update_inst,
                update_mask="capacity",
            )

            return LakebaseCapacityUpdateResponse(
                status="success",
                instance_name=instance_name,
                previous_capacity=previous_capacity,
                new_capacity=capacity,
                message=f"Capacity change initiated. The instance will scale to "
                f"{capacity}. This may take a few minutes to complete.",
            )

        return await asyncio.to_thread(update_capacity)
    except NotFound:
        return McpErrorResponse(error=f"Database instance '{instance_name}' not found.")
    except Exception as e:
        return McpErrorResponse(error=f"Failed to set instance capacity: {e!s}")


# ============================================================================
# MCP Tools - SQL Execution
# ============================================================================


@mcp.tool()
async def run_lakebase_sql(
    instance_name: str,
    sql: str,
    database_name: str = DEFAULT_DATABASE_NAME,
    read_only: bool = True,
) -> LakebaseSqlResult | McpErrorResponse:
    """Execute SQL against a Lakebase database instance.

    By default, runs in read-only mode for safety. Set read_only=False for
    write operations (INSERT, UPDATE, DELETE, DDL).

    Args:
        instance_name: Name of the database instance
        sql: SQL statement to execute
        database_name: Database name within the instance (default: databricks_postgres)
        read_only: If True, executes in a read-only transaction (default: True)

    Returns:
        LakebaseSqlResult with query results or affected row count

    Examples:
        - SELECT queries: Returns columns and rows
        - INSERT/UPDATE/DELETE: Returns rows_affected count (requires read_only=False)
        - DDL statements: Returns success status (requires read_only=False)
    """
    try:
        ws = _get_workspace_client()

        def execute_sql() -> LakebaseSqlResult:
            engine = _get_lakebase_engine(ws, instance_name, database_name)

            with engine.connect() as conn:
                if read_only:
                    # Start a read-only transaction
                    conn.execute(text("SET TRANSACTION READ ONLY"))

                result = conn.execute(text(sql))

                if result.returns_rows:
                    columns = list(result.keys())
                    rows = [dict(zip(columns, row)) for row in result.fetchall()]
                    return LakebaseSqlResult(
                        success=True,
                        columns=columns,
                        rows=rows,  # type: ignore[arg-type]
                    )
                else:
                    if not read_only:
                        conn.commit()
                    return LakebaseSqlResult(
                        success=True,
                        rows_affected=result.rowcount,
                    )

        return await asyncio.to_thread(execute_sql)
    except NotFound:
        return McpErrorResponse(error=f"Database instance '{instance_name}' not found.")
    except Exception as e:
        error_msg = str(e)
        # Check if it's a read-only transaction error
        if "read-only transaction" in error_msg.lower() and read_only:
            return LakebaseSqlResult(
                success=False,
                error=f"Cannot execute write operation in read-only mode. "
                f"Set read_only=False to allow writes. Original error: {error_msg}",
            )
        return LakebaseSqlResult(success=False, error=error_msg)


# ============================================================================
# MCP Tools - Schema Introspection
# ============================================================================


@mcp.tool()
async def get_lakebase_tables(
    instance_name: str,
    database_name: str = DEFAULT_DATABASE_NAME,
) -> LakebaseTablesResponse | McpErrorResponse:
    """List all tables in a Lakebase database.

    Returns tables from all schemas except system schemas (pg_catalog,
    information_schema).

    Args:
        instance_name: Name of the database instance
        database_name: Database name within the instance (default: databricks_postgres)

    Returns:
        LakebaseTablesResponse with list of tables
    """
    try:
        ws = _get_workspace_client()

        def fetch_tables() -> LakebaseTablesResponse:
            engine = _get_lakebase_engine(ws, instance_name, database_name)

            query = """
                SELECT
                    table_schema,
                    table_name,
                    table_type
                FROM information_schema.tables
                WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                ORDER BY table_schema, table_name
            """

            with engine.connect() as conn:
                result = conn.execute(text(query))
                tables = [
                    LakebaseTableInfo(
                        table_schema=row[0],
                        table_name=row[1],
                        table_type=row[2],
                    )
                    for row in result.fetchall()
                ]

            return LakebaseTablesResponse(
                instance_name=instance_name,
                database_name=database_name,
                tables=tables,
                total_count=len(tables),
            )

        return await asyncio.to_thread(fetch_tables)
    except NotFound:
        return McpErrorResponse(error=f"Database instance '{instance_name}' not found.")
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get tables: {e!s}")


@mcp.tool()
async def describe_lakebase_table(
    instance_name: str,
    table_name: str,
    table_schema: str = "public",
    database_name: str = DEFAULT_DATABASE_NAME,
) -> LakebaseTableSchemaResponse | McpErrorResponse:
    """Get detailed schema information for a table in a Lakebase database.

    Returns column definitions, primary key, and index information.

    Args:
        instance_name: Name of the database instance
        table_name: Name of the table to describe
        table_schema: Schema containing the table (default: public)
        database_name: Database name within the instance (default: databricks_postgres)

    Returns:
        LakebaseTableSchemaResponse with column and constraint details
    """
    try:
        ws = _get_workspace_client()

        def fetch_schema() -> LakebaseTableSchemaResponse:
            engine = _get_lakebase_engine(ws, instance_name, database_name)

            # Query for column information
            columns_query = """
                SELECT
                    column_name,
                    data_type,
                    is_nullable,
                    column_default,
                    ordinal_position
                FROM information_schema.columns
                WHERE table_schema = :schema AND table_name = :table
                ORDER BY ordinal_position
            """

            # Query for primary key columns
            pk_query = """
                SELECT kcu.column_name
                FROM information_schema.table_constraints tc
                JOIN information_schema.key_column_usage kcu
                    ON tc.constraint_name = kcu.constraint_name
                    AND tc.table_schema = kcu.table_schema
                WHERE tc.constraint_type = 'PRIMARY KEY'
                    AND tc.table_schema = :schema
                    AND tc.table_name = :table
                ORDER BY kcu.ordinal_position
            """

            # Query for indexes
            indexes_query = """
                SELECT indexname
                FROM pg_indexes
                WHERE schemaname = :schema AND tablename = :table
            """

            with engine.connect() as conn:
                # Fetch columns
                result = conn.execute(
                    text(columns_query), {"schema": table_schema, "table": table_name}
                )
                columns = [
                    LakebaseColumnInfo(
                        column_name=row[0],
                        data_type=row[1],
                        is_nullable=row[2] == "YES",
                        column_default=row[3],
                        ordinal_position=row[4],
                    )
                    for row in result.fetchall()
                ]

                if not columns:
                    raise ValueError(
                        f"Table '{table_schema}.{table_name}' not found or has no columns"
                    )

                # Fetch primary key
                pk_result = conn.execute(
                    text(pk_query), {"schema": table_schema, "table": table_name}
                )
                primary_key = [row[0] for row in pk_result.fetchall()]

                # Fetch indexes
                idx_result = conn.execute(
                    text(indexes_query), {"schema": table_schema, "table": table_name}
                )
                indexes = [row[0] for row in idx_result.fetchall()]

            return LakebaseTableSchemaResponse(
                instance_name=instance_name,
                database_name=database_name,
                table_schema=table_schema,
                table_name=table_name,
                columns=columns,
                primary_key=primary_key,
                indexes=indexes,
            )

        return await asyncio.to_thread(fetch_schema)
    except NotFound:
        return McpErrorResponse(error=f"Database instance '{instance_name}' not found.")
    except ValueError as e:
        return McpErrorResponse(error=str(e))
    except Exception as e:
        return McpErrorResponse(error=f"Failed to describe table: {e!s}")
