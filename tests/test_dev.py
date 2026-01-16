import json
import socket
import time
from http.client import HTTPResponse
from pathlib import Path
from typing import TypedDict, cast
from urllib.request import urlopen

from apx._core import run_cli


class DevLock(TypedDict):
    port: int


class HealthPayload(TypedDict):
    status: str


def wait_for_port_free(host: str, port: int, timeout_seconds: float = 10.0) -> None:
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            try:
                sock.settimeout(0.2)
                if sock.connect_ex((host, port)) != 0:
                    return
            except OSError:
                time.sleep(0.1)
    raise AssertionError(f"Port {port} on {host} did not become free")


def test_dev_server_start_stop(tmp_path: Path) -> None:
    start_code = run_cli(["apx", "dev", "start", str(tmp_path)])
    assert start_code == 0

    lock_path = tmp_path / ".apx" / "dev.lock"
    assert lock_path.exists()
    lock_data = cast(DevLock, json.loads(lock_path.read_text()))
    port = int(lock_data["port"])
    host = "127.0.0.1"

    with urlopen(f"http://{host}:{port}/_apx/health", timeout=2) as response:  # pyright: ignore[reportAny]
        response_obj = cast(HTTPResponse, response)
        assert response_obj.status == 200
        payload = cast(HealthPayload, json.loads(response_obj.read().decode("utf-8")))
        assert payload["status"] == "ok"

    try:
        stop_code = run_cli(["apx", "dev", "stop", str(tmp_path)])
        assert stop_code == 0
    finally:
        wait_for_port_free(host, port)
