import socket
import asyncio
import json
from pathlib import Path

import httpx
from tenacity import (
    retry,
    stop_after_attempt,
    wait_exponential,
    retry_if_exception_type,
)

from conftest import run_cli_async, _init_project, run_cli_background


async def test_stateful_dev_server_crud(tmp_path: Path) -> None:
    """Test stateful template with dev server CRUD operations.

    This integration test verifies that:
    1. A stateful project can start with PGLite (via APX_DEV_DB_PORT)
    2. The runtime correctly picks up the dev DB port
    3. CRUD operations work end-to-end through the database
    """
    project_path = tmp_path / "project"
    project_path.mkdir(parents=True, exist_ok=True)
    _init_project(project_path, template="stateful")

    test_failed = False
    failure_exception: BaseException | None = None

    try:
        # Start dev server
        print(f"Starting dev server in {project_path}")
        start_result = await run_cli_async(["dev", "start"], cwd=project_path)
        assert start_result.returncode == 0, f"Failed to start dev server: {start_result.stderr}"

        # Read dev.lock to get the dev server port
        dev_lock_path = project_path / ".apx" / "dev.lock"
        dev_lock = json.loads(dev_lock_path.read_text())
        dev_port = dev_lock["port"]
        print(f"Dev server started at http://localhost:{dev_port}")

        # Wait for services to initialize (PGLite + backend)
        await asyncio.sleep(4)

        # Inject CRUD model and router into the project
        # First, update models.py with a SQLModel table
        models_code = '''from typing import Optional
from pydantic import BaseModel
from sqlmodel import SQLModel, Field
from .. import __version__


class VersionOut(BaseModel):
    version: str

    @classmethod
    def from_metadata(cls):
        return cls(version=__version__)


class Item(SQLModel, table=True):
    id: Optional[int] = Field(default=None, primary_key=True)
    name: str
    description: str = ""
'''

        # Then, update router.py with CRUD endpoints
        router_code = '''from typing import List
from fastapi import APIRouter
from sqlmodel import select
from .models import VersionOut, Item
from .dependencies import SessionDep
from .._metadata import api_prefix

api = APIRouter(prefix=api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()


@api.post("/items", response_model=Item)
def create_item(item: Item, session: SessionDep):
    session.add(item)
    session.commit()
    session.refresh(item)
    return item


@api.get("/items", response_model=List[Item])
def list_items(session: SessionDep):
    return session.exec(select(Item)).all()
'''

        # Write the updated files
        backend_path = project_path / "src" / "test_app" / "backend"
        (backend_path / "models.py").write_text(models_code)
        (backend_path / "router.py").write_text(router_code)
        print("Injected CRUD model and router, waiting for hot reload...")

        # Wait for hot reload to pick up changes
        await asyncio.sleep(4)

        # Set up HTTP client with retry logic
        http_client = httpx.AsyncClient()

        with_retry = retry(
            stop=stop_after_attempt(5),
            wait=wait_exponential(multiplier=1, min=1, max=10),
            retry=retry_if_exception_type((httpx.RequestError, httpx.HTTPStatusError)),
        )

        @with_retry
        async def create_item() -> httpx.Response:
            resp = await http_client.post(
                f"http://localhost:{dev_port}/api/items",
                json={"name": "Test Item", "description": "A test item"}
            )
            resp.raise_for_status()
            return resp

        @with_retry
        async def list_items() -> httpx.Response:
            resp = await http_client.get(f"http://localhost:{dev_port}/api/items")
            resp.raise_for_status()
            return resp

        # Test CREATE operation
        print("Testing CREATE: POST /api/items")
        create_resp = await create_item()
        assert create_resp.status_code == 200
        item = create_resp.json()
        assert item["name"] == "Test Item"
        assert item["description"] == "A test item"
        assert item["id"] is not None
        print(f"Created item with id={item['id']}")

        # Test READ operation
        print("Testing READ: GET /api/items")
        list_resp = await list_items()
        assert list_resp.status_code == 200
        items = list_resp.json()
        assert len(items) == 1, f"Expected 1 item, got {len(items)}"
        assert items[0]["name"] == "Test Item"
        assert items[0]["id"] == item["id"]
        print(f"Listed {len(items)} item(s) successfully")

        # Verify logs show dev DB connection
        logs_result = await run_cli_async(["dev", "logs"], cwd=project_path)
        assert "Using local dev database" in logs_result.stdout, (
            "Expected 'Using local dev database' in logs"
        )
        print("Verified logs contain 'Using local dev database'")

        await http_client.aclose()

    except Exception as e:
        test_failed = True
        failure_exception = e

    finally:
        if test_failed:
            print("\n" + "=" * 60)
            print("TEST FAILED - Collecting debug logs")
            print("=" * 60)

            # Collect dev server logs
            print("\n--- Dev Server Logs ---")
            logs_result = await run_cli_async(["dev", "logs"], cwd=project_path)
            print(f"logs returncode: {logs_result.returncode}")
            if logs_result.stdout:
                print("stdout:")
                for line in logs_result.stdout.split("\n"):
                    print(f"  {line}")
            if logs_result.stderr:
                print("stderr:")
                for line in logs_result.stderr.split("\n"):
                    print(f"  {line}")

            # Check dev.lock contents
            dev_lock_path = project_path / ".apx" / "dev.lock"
            if dev_lock_path.exists():
                print("\n--- dev.lock contents ---")
                print(dev_lock_path.read_text())

            # Check status
            print("\n--- Dev Server Status ---")
            status_result = await run_cli_async(["dev", "status"], cwd=project_path)
            print(f"status returncode: {status_result.returncode}")
            if status_result.stdout:
                print(f"stdout: {status_result.stdout}")
            if status_result.stderr:
                print(f"stderr: {status_result.stderr}")

            print("=" * 60 + "\n")

        # Always stop the dev server
        print("Stopping dev server as a cleanup step")
        stop_result = await run_cli_async(["dev", "stop"], cwd=project_path)
        print(
            f"cleanup stop result: returncode={stop_result.returncode} "
            f"with error: {stop_result.stderr} and output: {stop_result.stdout}"
        )

        if test_failed and failure_exception:
            raise failure_exception


async def test_db_basic_connectivity():
    """Test basic PGLite database connectivity.

    This test verifies that:
    1. PGLite can be started via bun
    2. We can connect to it using psycopg/SQLModel
    3. Basic SQL queries work (SELECT 1)
    """
    from sqlalchemy import Engine, create_engine, text
    from sqlmodel import Session

    # Find a free port
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        port = s.getsockname()[1]

    print(f"Starting PGLite on port {port}")

    # Start PGLite database server
    async with run_cli_background(
        ["bun", "x", "@electric-sql/pglite-socket", "--db=memory://", f"--port={port}"],
    ) as proc:
        # Wait for the database to start
        max_wait = 10
        connected = False
        last_error: Exception | None = None
        engine: Engine | None = None

        for attempt in range(max_wait * 2):  # Check every 0.5s
            await asyncio.sleep(0.5)

            # Check if process died
            if proc.returncode is not None:
                stdout = await proc.stdout.read() if proc.stdout else b""
                stderr = await proc.stderr.read() if proc.stderr else b""
                raise RuntimeError(
                    f"PGLite process exited with code {proc.returncode}. "
                    f"stdout: {stdout.decode()}, stderr: {stderr.decode()}"
                )

            # Try to connect
            try:
                # PGLite requires: user=postgres, password=postgres, database=postgres
                # sslmode=disable is required because PGLite doesn't support SSL
                engine = create_engine(
                    f"postgresql+psycopg://postgres:postgres@localhost:{port}/postgres?sslmode=disable",
                    pool_size=1,
                )
                with Session(engine) as session:
                    result = session.connection().execute(text("SELECT 1"))
                    value = result.scalar()
                    assert value == 1, f"Expected SELECT 1 to return 1, got {value}"
                    print(f"Successfully connected to PGLite on attempt {attempt + 1}")
                    connected = True
                    break
            except Exception as e:
                last_error = e
                print(f"Attempt {attempt + 1}: Connection failed - {e}")

        if not connected or engine is None:
            raise RuntimeError(
                f"Failed to connect to PGLite after {max_wait}s. Last error: {last_error}"
            )

        # Run a few more queries to verify stability
        print("Running additional verification queries...")
        with Session(engine) as session:
            # Create a test table
            session.connection().execute(
                text("CREATE TABLE IF NOT EXISTS test_table (id SERIAL PRIMARY KEY, name TEXT)")
            )
            session.commit()

            # Insert a row
            session.connection().execute(
                text("INSERT INTO test_table (name) VALUES ('test')")
            )
            session.commit()

            # Query the row
            result = session.connection().execute(text("SELECT name FROM test_table"))
            name = result.scalar()
            assert name == "test", f"Expected 'test', got {name}"

        print("All database operations completed successfully!")

        # Clean up SQLAlchemy engine to release connections before process termination
        print("[test] Disposing engine...")
        engine.dispose()
        print("[test] Engine disposed, exiting context manager...")
