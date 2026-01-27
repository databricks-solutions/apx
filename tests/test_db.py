import socket
import asyncio
import json
from pathlib import Path

import httpx

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
        if start_result.returncode != 0:
            for line in start_result.stderr.split("\n"):
                print(f"stderr: {line}")
            for line in start_result.stdout.split("\n"):
                print(f"stdout: {line}")
            raise RuntimeError(f"Failed to start dev server: {start_result.stderr}")

        # Read dev.lock to get the dev server port
        dev_lock_path = project_path / ".apx" / "dev.lock"
        dev_lock = json.loads(dev_lock_path.read_text())
        dev_port = dev_lock["port"]
        print(f"Dev server started at http://localhost:{dev_port}")

        # Wait for services to initialize (PGLite + backend)
        await asyncio.sleep(4)

        # Inject CRUD model and router into the project
        # First, update models.py with a SQLModel table
        # Note: Use separate ItemCreate model for request body (SQLModel best practice)
        # Note: Use Pydantic BaseModel for request body (ItemCreate) to ensure
        # proper FastAPI body parsing. SQLModel can have compatibility issues
        # with FastAPI's automatic body detection.
        models_code = """from typing import Optional
from pydantic import BaseModel
from sqlmodel import SQLModel, Field
from .. import __version__


class VersionOut(BaseModel):
    version: str

    @classmethod
    def from_metadata(cls):
        return cls(version=__version__)


class ItemCreate(BaseModel):
    name: str
    description: str = ""


class Item(SQLModel, table=True):
    id: Optional[int] = Field(default=None, primary_key=True)
    name: str
    description: str = ""
"""

        # Then, update router.py with CRUD endpoints
        router_code = """from typing import List
from fastapi import APIRouter
from sqlmodel import select
from .models import VersionOut, Item, ItemCreate
from .dependencies import SessionDep
from .._metadata import api_prefix

api = APIRouter(prefix=api_prefix)


@api.get("/version", response_model=VersionOut, operation_id="version")
async def version():
    return VersionOut.from_metadata()


@api.post("/items", response_model=Item)
def create_item(item: ItemCreate, session: SessionDep):
    db_item = Item(name=item.name, description=item.description)
    session.add(db_item)
    session.commit()
    session.refresh(db_item)
    return db_item


@api.get("/items", response_model=List[Item])
def list_items(session: SessionDep):
    return session.exec(select(Item)).all()
"""

        # Write the updated files
        backend_path = project_path / "src" / "test_app" / "backend"
        (backend_path / "models.py").write_text(models_code)
        (backend_path / "router.py").write_text(router_code)
        print("Injected CRUD model and router, waiting for hot reload...")

        # Wait for hot reload to pick up changes
        await asyncio.sleep(4)

        # Set up HTTP client with retry logic
        http_client = httpx.AsyncClient()

        async def create_item() -> httpx.Response:
            resp = await http_client.post(
                f"http://localhost:{dev_port}/api/items",
                json={"name": "Test Item", "description": "A test item"},
            )
            if resp.status_code == 422:
                print(f"422 Validation Error Response: {resp.json()}")
            resp.raise_for_status()
            return resp

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


async def test_db_parallel_connections():
    """Test SQLAlchemy connection pooling with PGLite - mixed read/write workload.

    This test verifies how well SQLAlchemy's connection pooling works with PGLite
    under a mixed workload of concurrent readers and writers.

    - 5 writer tasks: INSERT items into the database
    - 5 reader tasks: SELECT items from the database

    Each task holds its connection for 1-2 seconds to simulate real work.
    Results are collected and summarized to understand pooling behavior.
    """
    import random
    import time
    from dataclasses import dataclass
    from typing import Optional
    from sqlalchemy import create_engine, text, Engine, event
    from sqlmodel import SQLModel, Field, Session, select

    # Define Item model for read/write tests
    class Item(SQLModel, table=True):
        id: Optional[int] = Field(default=None, primary_key=True)
        name: str
        description: str = ""

    @dataclass
    class TaskResult:
        task_id: int
        task_type: str  # "writer" or "reader"
        success: bool
        duration_ms: float
        wait_for_conn_ms: float = 0
        items_affected: int = 0  # items written or read
        error: str | None = None

    @dataclass
    class PoolStats:
        checkouts: int = 0
        checkins: int = 0
        connects: int = 0
        disconnects: int = 0

    async def run_writer_task(
        task_id: int, engine: Engine, hold_time: float
    ) -> TaskResult:
        """Writer task: INSERT an item into the database."""
        start_time = time.monotonic()
        wait_start = start_time

        try:
            with Session(engine) as session:
                wait_ms = (time.monotonic() - wait_start) * 1000

                # Hold connection open for the specified time
                await asyncio.sleep(hold_time)

                # Create and insert an item
                item = Item(
                    name=f"Item from writer {task_id}",
                    description=f"Created by writer task {task_id} at {time.time()}",
                )
                session.add(item)
                session.commit()
                session.refresh(item)
                print(f"  Writer {task_id}: inserted item id={item.id}")

            duration_ms = (time.monotonic() - start_time) * 1000
            return TaskResult(
                task_id=task_id,
                task_type="writer",
                success=True,
                duration_ms=duration_ms,
                wait_for_conn_ms=wait_ms,
                items_affected=1,
            )
        except Exception as e:
            duration_ms = (time.monotonic() - start_time) * 1000
            return TaskResult(
                task_id=task_id,
                task_type="writer",
                success=False,
                duration_ms=duration_ms,
                error=str(e),
            )

    async def run_reader_task(
        task_id: int, engine: Engine, hold_time: float
    ) -> TaskResult:
        """Reader task: SELECT all items from the database."""
        start_time = time.monotonic()
        wait_start = start_time

        try:
            with Session(engine) as session:
                wait_ms = (time.monotonic() - wait_start) * 1000

                # Hold connection open for the specified time
                await asyncio.sleep(hold_time)

                # Query all items
                items = session.exec(select(Item)).all()
                print(f"  Reader {task_id}: read {len(items)} items")

            duration_ms = (time.monotonic() - start_time) * 1000
            return TaskResult(
                task_id=task_id,
                task_type="reader",
                success=True,
                duration_ms=duration_ms,
                wait_for_conn_ms=wait_ms,
                items_affected=len(items),
            )
        except Exception as e:
            duration_ms = (time.monotonic() - start_time) * 1000
            return TaskResult(
                task_id=task_id,
                task_type="reader",
                success=False,
                duration_ms=duration_ms,
                error=str(e),
            )

    # Find a free port
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        port = s.getsockname()[1]

    print(f"Starting PGLite on port {port}")

    # Start PGLite database server
    async with run_cli_background(
        ["bun", "x", "@electric-sql/pglite-socket", "--db=memory://", f"--port={port}"],
    ) as proc:
        # Wait for the database to start by polling
        max_wait = 10
        ready = False

        for attempt in range(max_wait * 2):
            await asyncio.sleep(0.5)

            if proc.returncode is not None:
                stdout = await proc.stdout.read() if proc.stdout else b""
                stderr = await proc.stderr.read() if proc.stderr else b""
                raise RuntimeError(
                    f"PGLite process exited with code {proc.returncode}. "
                    f"stdout: {stdout.decode()}, stderr: {stderr.decode()}"
                )

            try:
                test_engine = create_engine(
                    f"postgresql+psycopg://postgres:postgres@localhost:{port}/postgres?sslmode=disable",
                    pool_size=1,
                )
                with Session(test_engine) as session:
                    session.connection().execute(text("SELECT 1"))
                test_engine.dispose()
                print(f"PGLite ready after {attempt + 1} attempts")
                ready = True
                break
            except Exception:
                pass

        if not ready:
            raise RuntimeError(f"PGLite failed to start after {max_wait}s")

        # Create a shared engine with connection pooling
        pool_size = 5
        max_overflow = 5  # Total max connections = pool_size + max_overflow = 10
        pool_timeout = 30

        print(
            f"\nCreating shared engine with pool_size={pool_size}, max_overflow={max_overflow}"
        )

        engine = create_engine(
            f"postgresql+psycopg://postgres:postgres@localhost:{port}/postgres?sslmode=disable",
            pool_size=pool_size,
            max_overflow=max_overflow,
            pool_timeout=pool_timeout,
            pool_pre_ping=True,
        )

        # Create the items table
        print("Creating items table...")
        SQLModel.metadata.create_all(engine)

        # Track pool events
        stats = PoolStats()

        @event.listens_for(engine, "checkout")
        def on_checkout(dbapi_conn, connection_record, connection_proxy):
            stats.checkouts += 1

        @event.listens_for(engine, "checkin")
        def on_checkin(dbapi_conn, connection_record):
            stats.checkins += 1

        @event.listens_for(engine, "connect")
        def on_connect(dbapi_conn, connection_record):
            stats.connects += 1
            print(f"  [pool] New connection created (total: {stats.connects})")

        @event.listens_for(engine, "close")
        def on_close(dbapi_conn, connection_record):
            stats.disconnects += 1

        # Launch 5 writers and 5 readers in parallel
        num_writers = 5
        num_readers = 5
        print(f"\nLaunching {num_writers} writers and {num_readers} readers...")

        tasks = []

        # Create writer tasks
        for i in range(num_writers):
            hold_time = random.uniform(1.0, 2.0)
            print(f"  Writer {i}: will hold connection for {hold_time:.2f}s")
            tasks.append(run_writer_task(i, engine, hold_time))

        # Create reader tasks
        for i in range(num_readers):
            hold_time = random.uniform(1.0, 2.0)
            print(f"  Reader {i}: will hold connection for {hold_time:.2f}s")
            tasks.append(run_reader_task(i, engine, hold_time))

        # Run all tasks in parallel
        results: list[TaskResult] = await asyncio.gather(*tasks)

        # Print summary
        print("\n" + "=" * 60)
        print("SQLALCHEMY POOLING WITH PGLITE - READ/WRITE TEST SUMMARY")
        print("=" * 60)

        print("\nPool Configuration:")
        print(f"  pool_size: {pool_size}")
        print(f"  max_overflow: {max_overflow}")
        print(f"  pool_timeout: {pool_timeout}s")

        print("\nPool Statistics:")
        print(f"  New connections created: {stats.connects}")
        print(f"  Connection checkouts: {stats.checkouts}")
        print(f"  Connection checkins: {stats.checkins}")
        reuse_rate = (stats.checkouts - stats.connects) / max(stats.checkouts, 1) * 100
        print(f"  Connection reuse rate: {reuse_rate:.1f}%")

        # Separate results by type
        writers = [r for r in results if r.task_type == "writer"]
        readers = [r for r in results if r.task_type == "reader"]

        successful_writers = [r for r in writers if r.success]
        failed_writers = [r for r in writers if not r.success]
        successful_readers = [r for r in readers if r.success]
        failed_readers = [r for r in readers if not r.success]

        print(f"\nWriter Results ({num_writers} tasks):")
        print(f"  Successful: {len(successful_writers)}")
        print(f"  Failed: {len(failed_writers)}")
        if successful_writers:
            total_written = sum(r.items_affected for r in successful_writers)
            print(f"  Total items written: {total_written}")
            for r in sorted(successful_writers, key=lambda x: x.task_id):
                print(
                    f"    Writer {r.task_id}: {r.duration_ms:.0f}ms, "
                    f"pool_wait={r.wait_for_conn_ms:.0f}ms"
                )
        if failed_writers:
            print("  Failed writers:")
            for r in failed_writers:
                print(f"    Writer {r.task_id}: {r.error}")

        print(f"\nReader Results ({num_readers} tasks):")
        print(f"  Successful: {len(successful_readers)}")
        print(f"  Failed: {len(failed_readers)}")
        if successful_readers:
            for r in sorted(successful_readers, key=lambda x: x.task_id):
                print(
                    f"    Reader {r.task_id}: read {r.items_affected} items, "
                    f"{r.duration_ms:.0f}ms, pool_wait={r.wait_for_conn_ms:.0f}ms"
                )
        if failed_readers:
            print("  Failed readers:")
            for r in failed_readers:
                print(f"    Reader {r.task_id}: {r.error}")

        # Overall summary
        total_success = len(successful_writers) + len(successful_readers)
        total_tasks = num_writers + num_readers
        print("\nOverall:")
        print(f"  Total tasks: {total_tasks}")
        print(f"  Successful: {total_success}")
        print(f"  Failed: {total_tasks - total_success}")
        print(f"  Success rate: {total_success / total_tasks * 100:.1f}%")

        print("\n" + "=" * 60)

        # Clean up
        print("\nDisposing engine...")
        engine.dispose()
        print(f"Final disconnects: {stats.disconnects}")
