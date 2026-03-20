"""Integration tests for the APX bench app running in a Docker container.

All tests hit the real APX server via httpx. The container is managed by the
session-scoped ``apx_container`` fixture in conftest.py.
"""

from __future__ import annotations

import docker.models.containers
import httpx
import pytest


# ---------------------------------------------------------------------------
# Health & meta
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestHealth:
    def test_echo(self, client: httpx.Client) -> None:
        r = client.get("/api/echo")
        assert r.status_code == 200
        assert r.json() == {"echo": True}

    def test_health(self, client: httpx.Client) -> None:
        r = client.get("/api/health")
        assert r.status_code == 200
        assert r.json() == {"status": "ok"}

    def test_version(self, client: httpx.Client) -> None:
        r = client.get("/api/version")
        assert r.status_code == 200
        body = r.json()
        assert "apx" in body
        assert isinstance(body["apx"], str)
        assert body["apx"] != ""


# ---------------------------------------------------------------------------
# Items CRUD
# ---------------------------------------------------------------------------


@pytest.fixture()
def _reset_items(client: httpx.Client) -> None:
    """Reset items to defaults before each CRUD test."""
    r = client.post("/api/items/reset")
    assert r.status_code == 200


@pytest.mark.integration
@pytest.mark.usefixtures("_reset_items")
class TestItemsCRUD:
    def test_list_items_default(self, client: httpx.Client) -> None:
        r = client.get("/api/items")
        assert r.status_code == 200
        items = r.json()
        assert isinstance(items, list)
        assert len(items) == 10
        assert items[0]["id"] == 1
        assert items[-1]["id"] == 10

    def test_get_item(self, client: httpx.Client) -> None:
        r = client.get("/api/items/1")
        assert r.status_code == 200
        item = r.json()
        assert item["id"] == 1
        assert item["name"] == "Item 1"
        assert isinstance(item["price"], float)
        assert isinstance(item["tags"], list)

    def test_get_item_not_found(self, client: httpx.Client) -> None:
        r = client.get("/api/items/9999")
        assert r.status_code == 404

    def test_create_item(self, client: httpx.Client) -> None:
        body = {"name": "Test Item", "price": 42.0, "tags": ["new"]}
        r = client.post("/api/items", json=body)
        assert r.status_code == 201
        item = r.json()
        assert item["name"] == "Test Item"
        assert item["price"] == 42.0
        assert item["tags"] == ["new"]
        assert "id" in item
        assert isinstance(item["id"], int)

    def test_update_item(self, client: httpx.Client) -> None:
        r = client.patch("/api/items/1", json={"name": "Updated"})
        assert r.status_code == 200
        item = r.json()
        assert item["id"] == 1
        assert item["name"] == "Updated"
        # Price should be unchanged from defaults.
        assert item["price"] == 9.99

    def test_update_item_not_found(self, client: httpx.Client) -> None:
        r = client.patch("/api/items/9999", json={"name": "nope"})
        assert r.status_code == 404

    def test_delete_item(self, client: httpx.Client) -> None:
        r = client.delete("/api/items/1")
        assert r.status_code == 204

        r = client.get("/api/items/1")
        assert r.status_code == 404

    def test_items_reset(self, client: httpx.Client) -> None:
        client.delete("/api/items/1")
        r = client.post("/api/items/reset")
        assert r.status_code == 200
        assert r.json() == {"status": "reset", "items": 10}

        r = client.get("/api/items/1")
        assert r.status_code == 200

    def test_crud_lifecycle(self, client: httpx.Client) -> None:
        # Create
        r = client.post(
            "/api/items", json={"name": "Lifecycle", "price": 1.0, "tags": ["a"]}
        )
        assert r.status_code == 201
        item_id = r.json()["id"]

        # Read
        r = client.get(f"/api/items/{item_id}")
        assert r.status_code == 200
        assert r.json()["name"] == "Lifecycle"

        # Update
        r = client.patch(
            f"/api/items/{item_id}", json={"name": "Updated Lifecycle", "price": 2.0}
        )
        assert r.status_code == 200
        updated = r.json()
        assert updated["name"] == "Updated Lifecycle"
        assert updated["price"] == 2.0
        assert updated["tags"] == ["a"]

        # Delete
        r = client.delete(f"/api/items/{item_id}")
        assert r.status_code == 204

        # Confirm gone
        r = client.get(f"/api/items/{item_id}")
        assert r.status_code == 404


# ---------------------------------------------------------------------------
# Scheduler / pipeline
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestScheduler:
    def test_yield_once(self, client: httpx.Client) -> None:
        r = client.get("/api/yield-once")
        assert r.status_code == 200
        assert r.json() == {"yielded": True}

    def test_cpu_work(self, client: httpx.Client) -> None:
        r = client.get("/api/cpu/1000")
        assert r.status_code == 200
        body = r.json()
        assert body["n"] == 1000
        expected = sum(i * i for i in range(1000))
        assert body["result"] == expected

    def test_cpu_cap(self, client: httpx.Client) -> None:
        r = client.get("/api/cpu/2000000")
        assert r.status_code == 200
        assert r.json()["n"] == 1_000_000

    def test_deps(self, client: httpx.Client) -> None:
        r = client.get("/api/deps")
        assert r.status_code == 200
        assert r.json() == {"chain": "a(b(c))"}


# ---------------------------------------------------------------------------
# Payload: large responses, upload, streaming
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestPayload:
    def test_large_response(self, client: httpx.Client) -> None:
        r = client.get("/api/large/10")
        assert r.status_code == 200
        assert r.headers["content-type"] == "text/plain; charset=utf-8"
        assert len(r.content) == 10 * 1024
        assert r.text == "x" * 10240

    def test_large_cap(self, client: httpx.Client) -> None:
        r = client.get("/api/large/2048")
        assert r.status_code == 200
        assert len(r.content) == 1024 * 1024

    def test_upload(self, client: httpx.Client) -> None:
        payload = b"y" * 2048
        r = client.post("/api/upload", content=payload)
        assert r.status_code == 200
        assert r.json() == {"size": 2048}

    def test_stream_response(self, client: httpx.Client) -> None:
        r = client.get("/api/stream/10")
        assert r.status_code == 200
        assert "text/plain" in r.headers["content-type"]
        lines = r.text.strip().split("\n")
        assert len(lines) == 10
        for i, line in enumerate(lines):
            assert line == f"chunk-{i}"

    def test_stream_cap(self, client: httpx.Client) -> None:
        r = client.get("/api/stream/20000")
        assert r.status_code == 200
        lines = r.text.strip().split("\n")
        assert len(lines) == 10_000


# ---------------------------------------------------------------------------
# X-Request-Id header
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestRequestId:
    def test_request_id_generated_when_absent(self, client: httpx.Client) -> None:
        """Framework generates a UUID v4 when no X-Request-Id is sent."""
        import uuid

        r = client.get("/api/request-id")
        assert r.status_code == 200
        rid = r.json()["request_id"]
        assert rid is not None, "X-Request-Id should be generated by the framework"
        parsed = uuid.UUID(rid)
        assert parsed.version == 4

    def test_request_id_preserved_when_present(self, client: httpx.Client) -> None:
        """Framework preserves the X-Request-Id sent by the caller."""
        sent_id = "560683e1-d1a2-4f0c-8bf7-dc5d91609233"
        r = client.get("/api/request-id", headers={"x-request-id": sent_id})
        assert r.status_code == 200
        assert r.json()["request_id"] == sent_id


# ---------------------------------------------------------------------------
# Static page
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestStatic:
    def test_static_index(self, client: httpx.Client) -> None:
        r = client.get("/")
        assert r.status_code == 200
        assert "<html" in r.text.lower()


# ---------------------------------------------------------------------------
# Container log hygiene
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestLogHygiene:
    """Verify the container logs are free of asyncio task-leak noise.

    The ``_guarded`` wrapper must forward app exceptions through the
    response channel *without* re-raising. Re-raising causes asyncio to
    log "Task exception was never retrieved" on every error — the exact
    spam observed in production bench-apx logs.

    This test deliberately exercises the stream-cap path (which closes
    the stream channel while the Python side is still sending) and then
    inspects the container logs.
    """

    def test_no_task_exception_never_retrieved(
        self,
        client: httpx.Client,
        container: docker.models.containers.Container,
    ) -> None:
        # Hit stream cap — Python side gets "stream channel closed"
        # when it tries to send more chunks than the cap allows.
        for _ in range(3):
            r = client.get("/api/stream/20000")
            assert r.status_code == 200

        import time

        time.sleep(1)

        logs = container.logs(tail=500).decode("utf-8", errors="replace")
        leaks = [
            line
            for line in logs.splitlines()
            if "Task exception was never retrieved" in line
        ]
        assert leaks == [], (
            f"_guarded is re-raising exceptions, causing asyncio log spam "
            f"({len(leaks)} occurrences):\n" + "\n".join(leaks[:5])
        )
