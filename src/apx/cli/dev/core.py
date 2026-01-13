"""Core dev server management - single source of truth for apx dev.

This module consolidates all dev server functionality:
- Process tracking and management
- Port utilities and availability checking
- Token management (OBO tokens via keyring)
- DevCore class for orchestrating start/stop/restart

Design goals:
- Only stop processes we started (tracked by pid + create_time)
- Use SIGTERM -> SIGKILL sequence for reliable termination
- Target process groups to catch child processes (vite/esbuild)
- Clean separation: dev server spawns frontend/backend as subprocesses
"""

from __future__ import annotations

import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Literal

import keyring
import psutil
from rich.table import Table
from typer import Exit

from apx import __version__
from apx.cli.dev.client import DevServerClient
from apx.cli.dev.logging import (
    DevLogComponent,
    get_logger,
    print_log_entry,
    suppress_output_and_logs,
)
from apx.constants import (
    BACKEND_PORT_END,
    BACKEND_PORT_START,
    DEV_SERVER_PORT_END,
    DEV_SERVER_PORT_START,
    FRONTEND_PORT_END,
    FRONTEND_PORT_START,
)
from apx.models import (
    ActionRequest,
    DevServerConfig,
    LogChannel,
    LogEntry,
    PortsConfig,
    ProjectConfig,
    StreamEvent,
    TrackedProcess,
)
from apx.utils import console, ensure_dir

if TYPE_CHECKING:
    from rich.status import Status

logger = get_logger(DevLogComponent.SERVER)
backend_logger = get_logger(DevLogComponent.BACKEND)


# =============================================================================
# Port Utilities
# =============================================================================


def is_port_available(
    port: int, host: str = "127.0.0.1", *, allow_reuse: bool = False
) -> bool:
    """Check if a port is available for binding.

    Uses multiple strategies to detect if a port is in use:
    1. Try connecting to the port (detects listening servers) on both IPv4 and IPv6
    2. Try binding to the port on both IPv4 and IPv6 addresses
    3. Check netstat-style connection listing for both IPv4 and IPv6

    Args:
        port: Port number to check
        host: Host to check on (default: 127.0.0.1)
        allow_reuse: If True, use SO_REUSEADDR when checking bind availability.
                    This allows detecting ports in TIME_WAIT as available,
                    matching what uvicorn does when actually binding.
                    Use this for restart scenarios.

    Returns:
        True if port is available, False otherwise
    """
    # Strategy 1: Try to connect on both IPv4 and IPv6
    # If we can connect, something is definitely listening

    # Check IPv4 (127.0.0.1)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.2)
            result = sock.connect_ex(("127.0.0.1", port))
            if result == 0:
                return False
    except (socket.error, OSError):
        pass

    # Check IPv6 (::1)
    try:
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.2)
            result = sock.connect_ex(("::1", port))
            if result == 0:
                return False
    except (socket.error, OSError):
        pass

    # Strategy 2: Try binding to both IPv4 and IPv6 addresses
    # A server can bind to either, so we must check both

    # Determine SO_REUSEADDR setting based on allow_reuse
    reuse_addr_value = 1 if allow_reuse else 0

    # Check IPv4: Try binding to BOTH 127.0.0.1 and 0.0.0.0
    for bind_host in ["127.0.0.1", "0.0.0.0"]:
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                sock.setsockopt(
                    socket.SOL_SOCKET, socket.SO_REUSEADDR, reuse_addr_value
                )
                if hasattr(socket, "SO_REUSEPORT"):
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 0)
                sock.bind((bind_host, port))
        except OSError as e:
            if e.errno in (48, 98, 13):  # Address in use / Permission denied
                return False
            return False

    # Check IPv6: Try binding to ::1 and ::
    for bind_host in ["::1", "::"]:
        try:
            with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
                sock.setsockopt(
                    socket.SOL_SOCKET, socket.SO_REUSEADDR, reuse_addr_value
                )
                if hasattr(socket, "SO_REUSEPORT"):
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 0)
                sock.bind((bind_host, port))
        except OSError as e:
            if e.errno in (48, 98, 13):
                return False
            return False

    # Strategy 3: Check system network connections as a fallback
    if not allow_reuse:
        try:
            for conn in psutil.net_connections(kind="inet"):
                if (
                    hasattr(conn, "laddr")
                    and conn.laddr
                    and hasattr(conn.laddr, "port")
                ):
                    if conn.laddr.port == port:
                        return False
        except (psutil.AccessDenied, PermissionError, AttributeError):
            pass

        try:
            for conn in psutil.net_connections(kind="inet6"):
                if (
                    hasattr(conn, "laddr")
                    and conn.laddr
                    and hasattr(conn.laddr, "port")
                ):
                    if conn.laddr.port == port:
                        return False
        except (psutil.AccessDenied, PermissionError, AttributeError):
            pass

    return True


def find_available_port(start: int, end: int, host: str = "127.0.0.1") -> int | None:
    """Find an available port in the given range.

    Args:
        start: Start of port range (inclusive)
        end: End of port range (inclusive)
        host: Host to check on (default: 127.0.0.1)

    Returns:
        Available port number or None if no port is available
    """
    for port in range(start, end + 1):
        if is_port_available(port, host):
            return port
    return None


def wait_for_port_free(
    *,
    is_port_available_fn: Callable[[int], bool],
    port: int,
    timeout: float = 10.0,
    poll: float = 0.1,
) -> bool:
    """Wait for a port to become free."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if is_port_available_fn(port):
                return True
        except Exception:
            pass
        time.sleep(poll)
    return False


def find_listeners_for_port(port: int) -> list[int]:
    """Return PIDs that have a LISTEN socket bound to the port (best-effort)."""
    pids: set[int] = set()
    access_denied = False
    try:
        for conn in psutil.net_connections(kind="inet"):
            if not conn.laddr:
                continue
            if getattr(conn.laddr, "port", None) != port:
                continue
            if conn.status != psutil.CONN_LISTEN:
                continue
            if conn.pid:
                pids.add(int(conn.pid))
    except (psutil.AccessDenied, PermissionError):
        access_denied = True
    except Exception:
        access_denied = True

    # Fallback: iterate processes for same-user processes
    if not pids and access_denied:
        for proc in psutil.process_iter(["pid"]):
            try:
                pid = int(proc.pid)
                try:
                    conns = proc.net_connections(kind="inet")
                except Exception:
                    conns = []
                for c in conns:
                    if not getattr(c, "laddr", None):
                        continue
                    if getattr(c.laddr, "port", None) != port:
                        continue
                    if getattr(c, "status", None) != psutil.CONN_LISTEN:
                        continue
                    pids.add(pid)
                    break
            except (psutil.NoSuchProcess, psutil.AccessDenied, OSError):
                continue
            except Exception:
                continue
    return sorted(pids)


# =============================================================================
# Process Tracking and Management
# =============================================================================


def _get_pgid_safe(pid: int) -> int | None:
    """Get process group ID safely (returns None on Windows)."""
    if os.name == "nt":
        return None
    try:
        return os.getpgid(pid)
    except Exception:
        return None


def track_process(pid: int) -> TrackedProcess | None:
    """Create a TrackedProcess for a running PID, recording create_time and pgid."""
    try:
        proc = psutil.Process(pid)
        return TrackedProcess(
            pid=pid,
            create_time=float(proc.create_time()),
            pgid=_get_pgid_safe(pid),
        )
    except Exception:
        return None


def validate_tracked(tp: TrackedProcess) -> psutil.Process | None:
    """Return a psutil.Process only if PID matches create_time (prevents PID reuse bugs)."""
    if tp.pid is None or tp.create_time is None:
        return None
    try:
        proc = psutil.Process(tp.pid)
        if abs(float(proc.create_time()) - float(tp.create_time)) > 0.001:
            return None
        return proc
    except Exception:
        return None


def list_descendants(tp: TrackedProcess) -> list[psutil.Process]:
    """List all descendant processes of a tracked process."""
    proc = validate_tracked(tp)
    if proc is None:
        return []
    try:
        return proc.children(recursive=True)
    except Exception:
        return []


def _send_posix_signal_group(pgid: int, sig: signal.Signals) -> None:
    """Send a signal to an entire process group (POSIX only)."""
    os.killpg(pgid, sig)


def _list_pgid_members(pgid: int) -> list[int]:
    """Return PIDs in a process group (POSIX only)."""
    if os.name == "nt":
        return []
    pids: list[int] = []
    for proc in psutil.process_iter(["pid"]):
        try:
            pid = int(proc.pid)
            if _get_pgid_safe(pid) == pgid:
                pids.append(pid)
        except Exception:
            continue
    return pids


def _wait_for_pgid_empty(pgid: int, timeout: float, poll: float = 0.1) -> bool:
    """Wait for a process group to be empty."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not _list_pgid_members(pgid):
            return True
        time.sleep(poll)
    return not _list_pgid_members(pgid)


def kill_process_group(
    pgid: int,
    *,
    sigterm_timeout: float = 1.0,
    sigkill_timeout: float = 1.0,
) -> bool:
    """Aggressively kill all processes in a process group with SIGTERM then SIGKILL.

    This is the primary method for stopping frontend processes (vite/esbuild) that
    may outlive their parent (bun).

    Args:
        pgid: Process group ID to kill
        sigterm_timeout: Seconds to wait after SIGTERM before escalating to SIGKILL
        sigkill_timeout: Seconds to wait after SIGKILL for processes to exit

    Returns:
        True if process group is empty after termination, False otherwise
    """
    if os.name == "nt":
        return True

    logger.debug(f"Killing process group pgid={pgid}")

    members_before = _list_pgid_members(pgid)
    if not members_before:
        logger.debug(f"Process group pgid={pgid} already empty")
        return True

    logger.debug(
        f"Process group pgid={pgid} has {len(members_before)} members: {members_before}"
    )

    # Send SIGTERM to entire process group
    try:
        _send_posix_signal_group(pgid, signal.SIGTERM)
        logger.debug(f"Sent SIGTERM to pgid={pgid}")
    except ProcessLookupError:
        return True
    except Exception as e:
        logger.debug(f"Failed to send SIGTERM to pgid={pgid}: {e}")

    # Wait for graceful exit
    if _wait_for_pgid_empty(pgid, sigterm_timeout, poll=0.1):
        logger.debug(f"Process group pgid={pgid} exited after SIGTERM")
        return True

    # Send SIGKILL
    remaining = _list_pgid_members(pgid)
    logger.debug(
        f"SIGTERM timeout; {len(remaining)} processes remain in pgid={pgid}: {remaining}"
    )

    try:
        _send_posix_signal_group(pgid, signal.SIGKILL)
        logger.debug(f"Sent SIGKILL to pgid={pgid}")
    except ProcessLookupError:
        return True
    except Exception as e:
        logger.debug(f"Failed to send SIGKILL to pgid={pgid}: {e}")

    # Wait for SIGKILL to take effect
    if _wait_for_pgid_empty(pgid, sigkill_timeout, poll=0.1):
        logger.debug(f"Process group pgid={pgid} exited after SIGKILL")
        return True

    # Kill stragglers individually
    stragglers = _list_pgid_members(pgid)
    if stragglers:
        logger.debug(f"Killing {len(stragglers)} stragglers individually: {stragglers}")
        for pid in stragglers:
            try:
                os.kill(pid, signal.SIGKILL)
            except Exception:
                pass
        time.sleep(0.2)

    final_members = _list_pgid_members(pgid)
    if final_members:
        logger.warning(
            f"Process group pgid={pgid} still has members after SIGKILL: {final_members}"
        )
        return False

    logger.debug(f"Process group pgid={pgid} fully terminated")
    return True


def _terminate_tree_posix(root: psutil.Process, timeout: float) -> None:
    """Terminate a process tree on POSIX (best-effort)."""
    children: list[psutil.Process]
    try:
        children = root.children(recursive=True)
    except Exception:
        children = []

    for c in children:
        try:
            c.terminate()
        except Exception:
            pass

    try:
        root.terminate()
    except Exception:
        pass

    _, alive = psutil.wait_procs(children + [root], timeout=timeout)
    if alive:
        for p in alive:
            try:
                p.kill()
            except Exception:
                pass
        psutil.wait_procs(alive, timeout=max(0.5, timeout / 2))


def _terminate_tree_windows(root: psutil.Process, timeout: float) -> None:
    """Terminate a process tree on Windows (best-effort graceful)."""
    children = []
    try:
        children = root.children(recursive=True)
    except Exception:
        children = []

    for c in children:
        try:
            c.terminate()
        except Exception:
            pass

    try:
        root.terminate()
    except Exception:
        pass

    _, alive = psutil.wait_procs(children + [root], timeout=timeout)
    if alive:
        for p in alive:
            try:
                p.kill()
            except Exception:
                pass
        psutil.wait_procs(alive, timeout=max(0.5, timeout / 2))


def stop_tracked_process(
    tp: TrackedProcess,
    *,
    name: str = "process",
    sigterm_timeout: float = 1.0,
    sigkill_timeout: float = 1.0,
) -> None:
    """Stop a tracked process and its children.

    Behavior:
    - POSIX: signal process group with SIGTERM -> SIGKILL (no SIGINT).
    - Windows: best-effort CTRL_BREAK_EVENT, then terminate/kill process tree.
    """
    if os.name == "nt":
        proc = validate_tracked(tp)
        if proc is None or tp.pid is None:
            return
        logger.debug(f"Stopping {name} pid={tp.pid}")
        try:
            os.kill(tp.pid, signal.CTRL_BREAK_EVENT)  # type: ignore[attr-defined]
        except Exception:
            pass
        time.sleep(0.1)
        _terminate_tree_windows(proc, timeout=sigterm_timeout + sigkill_timeout)
        return

    # POSIX: prefer process group for reliable shutdown
    pgid = tp.pgid or (_get_pgid_safe(tp.pid) if tp.pid is not None else None)

    logger.debug(f"Stopping {name} pid={tp.pid} pgid={pgid}")

    if pgid is not None:
        kill_process_group(
            pgid,
            sigterm_timeout=sigterm_timeout,
            sigkill_timeout=sigkill_timeout,
        )

    # Fallback: also try to terminate the tracked process tree directly
    proc = validate_tracked(tp)
    if proc is not None:
        try:
            children = proc.children(recursive=True)
        except Exception:
            children = []

        try:
            proc.terminate()
        except Exception:
            pass

        for child in children:
            try:
                child.terminate()
            except Exception:
                pass

        all_procs = [proc] + children
        _, alive = psutil.wait_procs(all_procs, timeout=sigterm_timeout)

        if alive:
            for p in alive:
                try:
                    p.kill()
                except Exception:
                    pass
            psutil.wait_procs(alive, timeout=sigkill_timeout)


def kill_pids(
    pids: list[int], *, name: str = "process", sig: signal.Signals = signal.SIGKILL
) -> int:
    """Best-effort kill for a list of PIDs (POSIX/Windows). Returns count attempted."""
    killed = 0
    for pid in pids:
        try:
            os.kill(pid, sig)
            killed += 1
        except Exception:
            continue
    if killed:
        logger.debug(f"Killed {killed} {name} pid(s) via {sig}")
    return killed


def pids_belong_to_app(
    pids: list[int],
    *,
    app_dir: Path,
    expected_pgid: int | None = None,
) -> list[int]:
    """Filter PIDs down to those that look like they're part of this app's dev frontend."""
    app_dir_resolved = str(app_dir.resolve())
    keep: list[int] = []
    for pid in pids:
        try:
            if expected_pgid is not None and os.name != "nt":
                try:
                    if _get_pgid_safe(pid) == expected_pgid:
                        keep.append(pid)
                        continue
                except Exception:
                    pass

            proc = psutil.Process(pid)
            try:
                cwd = proc.cwd()
                if cwd and str(Path(cwd).resolve()) == app_dir_resolved:
                    keep.append(pid)
                    continue
            except Exception:
                pass

            try:
                cmd = " ".join(proc.cmdline() or [])
                if app_dir_resolved in cmd:
                    keep.append(pid)
                    continue
            except Exception:
                pass
        except Exception:
            continue
    return keep


def wait_for_no_descendants(
    tp: TrackedProcess, *, timeout: float = 5.0, poll: float = 0.1
) -> bool:
    """Return True if tracked process is gone and has no remaining descendants."""
    if tp.pgid is not None and os.name != "nt":
        return _wait_for_pgid_empty(tp.pgid, timeout=timeout, poll=poll)

    deadline = time.time() + timeout
    while time.time() < deadline:
        proc = validate_tracked(tp)
        if proc is None:
            return True
        try:
            kids = proc.children(recursive=True)
        except Exception:
            kids = []
        if not kids and not proc.is_running():
            return True
        try:
            if proc.status() == psutil.STATUS_ZOMBIE:
                return True
        except Exception:
            pass
        time.sleep(poll)
    return False


def cleanup_dev_server_processes(app_dir: Path, silent: bool = False) -> int:
    """Last-resort: stop dev server processes for an app_dir by cmdline matching."""
    killed = 0
    app_dir_str = str(app_dir.resolve())
    for proc in psutil.process_iter(["pid", "cmdline"]):
        try:
            cmdline = proc.cmdline() or []
            cmdline_str = " ".join(cmdline)
            if (
                "apx" in cmdline_str
                and "dev" in cmdline_str
                and "_run_server" in cmdline_str
                and app_dir_str in cmdline_str
            ):
                tp = track_process(int(proc.pid))
                if tp is None:
                    continue
                stop_tracked_process(tp, name="dev-server")
                wait_for_no_descendants(tp, timeout=5.0, poll=0.1)
                killed += 1
        except (psutil.NoSuchProcess, psutil.AccessDenied, OSError):
            continue
        except Exception:
            continue

    if killed and not silent:
        logger.info(f"Cleaned up {killed} dev-server process(es) for {app_dir_str}")
    return killed


# =============================================================================
# Token Management
# =============================================================================


def save_token_to_keyring(keyring_id: str, token_value: str) -> None:
    """Save token to system keyring."""
    keyring.set_password("apx-dev", keyring_id, token_value)


def get_token_from_keyring(keyring_id: str) -> str | None:
    """Get token from system keyring."""
    return keyring.get_password("apx-dev", keyring_id)


def delete_token_from_keyring(keyring_id: str) -> None:
    """Delete token from system keyring."""
    try:
        keyring.delete_password("apx-dev", keyring_id)
    except Exception:
        pass


def save_token_id(app_dir: Path, token_id: str) -> None:
    """Save token ID to project.json."""
    project_json_path = app_dir / ".apx" / "project.json"
    ensure_dir(app_dir / ".apx")

    try:
        config = read_project_config(project_json_path)
    except (FileNotFoundError, Exception):
        config = ProjectConfig()

    config.dev.token_id = token_id
    write_project_config(project_json_path, config)


def get_token_id(app_dir: Path) -> str | None:
    """Get token ID from project.json."""
    project_json_path = app_dir / ".apx" / "project.json"

    if project_json_path.exists():
        try:
            config = read_project_config(project_json_path)
            return config.dev.token_id
        except Exception:
            pass

    return None


# =============================================================================
# Project Configuration
# =============================================================================


def read_project_config(file_path: Path) -> ProjectConfig:
    """Read project config from file."""
    if not file_path.exists():
        raise FileNotFoundError(f"Project config not found at {file_path}")

    data: dict[str, Any] = json.loads(file_path.read_text())

    # Migrate old config structure to new structure
    if "dev_server_pid" in data or "dev_server_port" in data or "token_id" in data:
        migrated_data = {
            "dev": {
                "token_id": data.get("token_id"),
                "pid": data.get("dev_server_pid"),
                "port": data.get("dev_server_port"),
            },
        }
        return ProjectConfig.model_validate(migrated_data)

    return ProjectConfig.model_validate(data)


def write_project_config(file_path: Path, config: ProjectConfig) -> None:
    """Write project config to file."""
    ensure_dir(file_path.parent)
    file_path.write_text(config.model_dump_json(indent=2))


# =============================================================================
# Databricks Credential Validation
# =============================================================================


def validate_databricks_credentials() -> bool:
    """Validate that Databricks credentials are valid and not expired.

    Returns:
        True if credentials are valid, False otherwise
    """
    from databricks.sdk import WorkspaceClient

    try:
        with suppress_output_and_logs():
            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
            ws.current_user.me()
        return True
    except Exception as e:
        error_str = str(e).lower()
        if (
            "invalid" in error_str
            or "token" in error_str
            or "401" in error_str
            or "403" in error_str
        ):
            return False
        raise


def create_obo_token(
    app_module_name: str,
    token_lifetime_seconds: int,
    status_context: "Status | None" = None,
) -> tuple[str, str]:
    """Create a new OBO token via Databricks API.

    Returns:
        Tuple of (token_id, token_value)
    """
    from databricks.sdk import WorkspaceClient

    if status_context:
        status_context.update("🔐 Creating new OBO token")

    with suppress_output_and_logs():
        ws = WorkspaceClient(product="apx/dev", product_version=__version__)
        new_token = ws.tokens.create(
            comment=f"dev token for {app_module_name}, created by apx",
            lifetime_seconds=token_lifetime_seconds,
        )

    assert new_token.token_info is not None
    assert new_token.token_info.token_id is not None
    assert new_token.token_value is not None

    if status_context:
        status_context.update("✅ Token created successfully")

    return new_token.token_info.token_id, new_token.token_value


def prepare_obo_token(
    cwd: Path,
    app_module_name: str,
    token_lifetime_seconds: int = 60 * 60 * 4,
    status_context: "Status | None" = None,
) -> str:
    """Prepare the On-Behalf-Of token for the backend server.

    Checks keyring and project.json for existing valid token, creates new one if needed.
    """
    from databricks.sdk import WorkspaceClient

    try:
        with suppress_output_and_logs():
            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    except Exception as e:
        console.print(f"[red]❌ Failed to initialize Databricks client: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
        )
        raise Exit(code=1)

    keyring_id = str(cwd.resolve())

    if status_context:
        status_context.update("🔍 Checking keyring for existing token")

    keyring_token = get_token_from_keyring(keyring_id)
    stored_token_id = get_token_id(cwd)

    if keyring_token and stored_token_id:
        if status_context:
            status_context.update("🔐 Validating existing token")

        with suppress_output_and_logs():
            user_tokens = ws.tokens.list()
            user_token = next(
                (token for token in user_tokens if token.token_id == stored_token_id),
                None,
            )

        if user_token and user_token.expiry_time:
            expiry_timestamp = user_token.expiry_time / 1000
            current_time = time.time()
            time_remaining = expiry_timestamp - current_time

            min_remaining_time = 60 * 60
            if time_remaining > min_remaining_time:
                if status_context:
                    status_context.update(
                        f"✅ Using existing token (expires in {int(time_remaining / 3600)} hours)"
                    )
                return keyring_token
            else:
                if status_context:
                    status_context.update("⚠️  Token expiring soon, rotating...")
        else:
            if status_context:
                status_context.update("⚠️  Token invalid, creating new one...")
    elif keyring_token:
        if status_context:
            status_context.update("⚠️  Token found but missing metadata, recreating...")
        delete_token_from_keyring(keyring_id)

    if status_context:
        status_context.update("🔐 Creating new OBO token")

    token_id, new_token = create_obo_token(
        app_module_name,
        token_lifetime_seconds,
        status_context=status_context,
    )

    save_token_to_keyring(keyring_id, new_token)
    save_token_id(cwd, token_id)
    if status_context:
        status_context.update("💾 Token stored securely in keyring")

    return new_token


# =============================================================================
# Backend Runner (for subprocess execution)
# =============================================================================


def run_backend_server(
    cwd: Path,
    app_module_name: str,
    host: str,
    backend_port: int,
    obo: bool = False,
) -> None:
    """Run the backend server with hot-reload support.

    This function is designed to be called from a subprocess via `_run_backend` command.
    It runs uvicorn with watchfiles for automatic reloading on code changes.

    Args:
        cwd: Current working directory (app directory)
        app_module_name: Module name for the FastAPI app (e.g., "myapp.backend.app:app")
        host: Host to bind to
        backend_port: Port to bind to
        obo: Whether to enable On-Behalf-Of token middleware
    """
    import asyncio
    import traceback

    import uvicorn
    import watchfiles
    from databricks.sdk import WorkspaceClient
    from dotenv import load_dotenv
    from fastapi import Request
    from fastapi.responses import JSONResponse
    from starlette.middleware.base import BaseHTTPMiddleware

    from apx.cli.dev.logging import log_channel, set_log_channel, reset_log_channel
    from apx.cli.dev.reloader import load_app as _load_app_from_reloader
    from apx.constants import ACCESS_TOKEN_HEADER_NAME, FORWARDED_USER_HEADER_NAME

    # Track if this is the first run
    first_run = True
    obo_token: str | None = None

    while True:
        try:
            # Reload message
            if not first_run:
                backend_logger.info("Detected file changes, reloading backend...")

            # Reload .env file on every iteration
            dotenv_file = cwd / ".env"
            if dotenv_file.exists():
                load_dotenv(dotenv_file, override=True)

            # Prepare OBO token (will reuse if still valid)
            if obo and first_run:
                obo_token = prepare_obo_token(cwd, app_module_name, status_context=None)

            elif obo:
                obo_token = prepare_obo_token(cwd, app_module_name, status_context=None)

            # Load/reload the app instance
            app_instance, _ = _load_app_from_reloader(
                app_module_name, reload=not first_run
            )

            # Regenerate OpenAPI schema and client if the schema changed
            from apx.cli.openapi import regenerate_openapi_if_changed

            try:
                regenerate_openapi_if_changed(app_instance, cwd, logger=backend_logger)
            except Exception as e:
                backend_logger.warning(f"OpenAPI regeneration failed: {e}")

            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
            user_id = ws.current_user.me().id
            assert user_id is not None, "User ID is not set"
            try:
                workspace_id = ws.config.host.split("-")[1].split(".")[0]
            except Exception:
                workspace_id = "placeholder"

            async def user_id_middleware(request: Request, call_next):
                user_id_header: tuple[bytes, bytes] = (
                    FORWARDED_USER_HEADER_NAME.encode(),
                    f"{user_id}@{workspace_id}".encode(),
                )
                request.headers.__dict__["_list"].append(user_id_header)
                return await call_next(request)

            app_instance.add_middleware(BaseHTTPMiddleware, dispatch=user_id_middleware)

            # Add OBO middleware if enabled
            if obo and obo_token:
                encoded_token = obo_token.encode()

                async def obo_middleware(request: Request, call_next):
                    token_header: tuple[bytes, bytes] = (
                        ACCESS_TOKEN_HEADER_NAME.encode(),
                        encoded_token,
                    )
                    request.headers.__dict__["_list"].append(token_header)
                    return await call_next(request)

                app_instance.add_middleware(BaseHTTPMiddleware, dispatch=obo_middleware)

            # Add exception handler for better error reporting
            async def _dev_exception_handler(
                request: Request, exc: Exception
            ) -> JSONResponse:
                import sys

                traceback.print_exception(
                    type(exc), exc, exc.__traceback__, file=sys.stderr
                )
                return JSONResponse(
                    status_code=500, content={"detail": "Internal Server Error"}
                )

            app_instance.add_exception_handler(Exception, _dev_exception_handler)

            # Add context middleware for log routing
            async def channel_context_middleware(request: Request, call_next):
                token = set_log_channel(LogChannel.APP)
                try:
                    return await call_next(request)
                finally:
                    reset_log_channel(token)

            app_instance.add_middleware(
                BaseHTTPMiddleware, dispatch=channel_context_middleware
            )

            uvicorn_config = uvicorn.Config(
                app=app_instance,
                host=host,
                port=backend_port,
                log_level="debug",
                log_config=None,  # Use our logging setup
            )

            uvicorn_server = uvicorn.Server(uvicorn_config)
            first_run = False

            async def _run_server_and_watch(srv: uvicorn.Server) -> None:
                async def serve(server_instance: uvicorn.Server) -> None:
                    try:
                        with log_channel(LogChannel.APP):
                            await server_instance.serve()
                    except asyncio.CancelledError:
                        pass

                srv_task = asyncio.create_task(serve(srv))

                async def watch_files() -> None:
                    async for _changes in watchfiles.awatch(
                        cwd, watch_filter=watchfiles.PythonFilter()
                    ):
                        return

                watch_task_inner = asyncio.create_task(watch_files())

                done, pending = await asyncio.wait(
                    [srv_task, watch_task_inner],
                    return_when=asyncio.FIRST_COMPLETED,
                )

                # Shutdown server gracefully
                srv.should_exit = True
                await asyncio.sleep(0.5)

                # Cancel remaining tasks
                for task in pending:
                    task.cancel()
                    try:
                        await task
                    except asyncio.CancelledError:
                        pass

                # If server task crashed, re-raise
                if srv_task in done:
                    exc = srv_task.exception()
                    if exc:
                        raise exc

            asyncio.run(_run_server_and_watch(uvicorn_server))

        except KeyboardInterrupt:
            backend_logger.info("Backend received shutdown signal")
            break

        except Exception:
            action = "load" if first_run else "reload"
            backend_logger.error(
                f"Failed to {action} app, waiting for file changes to retry...\n{traceback.format_exc()}"
            )
            # Wait for next file change before retrying instead of fixed 1 second
            # This avoids spamming errors while the user is still editing
            try:

                async def wait_for_change() -> None:
                    async for _changes in watchfiles.awatch(
                        cwd, watch_filter=watchfiles.PythonFilter()
                    ):
                        return

                asyncio.run(wait_for_change())
                backend_logger.info("Detected file changes, retrying...")
            except KeyboardInterrupt:
                backend_logger.info("Backend received shutdown signal")
                break


# =============================================================================
# DevCore Class - Main Orchestration
# =============================================================================


class DevCore:
    """Core dev server management - single source of truth.

    This class provides the primary interface for starting, stopping, and
    managing the development server. All CLI commands and MCP tools should
    go through this class.
    """

    def __init__(self, app_dir: Path):
        """Initialize DevCore with an app directory.

        Args:
            app_dir: The path to the application directory
        """
        self.app_dir: Path = app_dir
        self.apx_dir: Path = app_dir / ".apx"
        self.project_json_path: Path = self.apx_dir / "project.json"

    def get_or_create_config(self) -> ProjectConfig:
        """Get or create project configuration."""
        ensure_dir(self.apx_dir)

        if self.project_json_path.exists():
            try:
                return read_project_config(self.project_json_path)
            except Exception:
                pass

        config = ProjectConfig()
        write_project_config(self.project_json_path, config)
        return config

    def _get_dev_server_port(self) -> int | None:
        """Get the dev server port from project.json."""
        try:
            config = self.get_or_create_config()
            return config.dev.dev_server_port
        except Exception:
            return None

    def is_running(self) -> bool:
        """Check if the dev server is running by attempting to connect."""
        port = self._get_dev_server_port()
        if port is None:
            return False

        try:
            client = DevServerClient(port=port, timeout=2.0)
            return client.is_running()
        except Exception:
            return False

    def start(
        self,
        config: DevServerConfig | None = None,
        preferred_ports: PortsConfig | None = None,
    ) -> None:
        """Start development server in detached mode.

        Args:
            config: Development server configuration. If not provided, uses defaults.
            preferred_ports: Optional preferred ports to try first.
        """
        if config is None:
            config = DevServerConfig()

        if self.is_running():
            console.print(
                "[yellow]⚠️  Dev server is already running. Run 'apx dev stop' first.[/yellow]"
            )
            raise Exit(code=1)

        # Clean up stale dev server if needed
        project_config = self.get_or_create_config()
        if project_config.dev.dev_server_pid is not None:
            console.print(
                "[cyan]🧹 Found previous dev session state; attempting safe cleanup...[/cyan]"
            )
            tp = track_process(project_config.dev.dev_server_pid)
            if tp is not None:
                stop_tracked_process(
                    tp,
                    name="dev-server",
                    sigterm_timeout=0.6,
                    sigkill_timeout=0.6,
                )
                wait_for_no_descendants(tp, timeout=2.0, poll=0.1)

        # Find available ports
        console.print("[cyan]🔍 Finding available ports...[/cyan]")

        available_dev_port: int | None = None
        available_frontend_port: int | None = None
        available_backend_port: int | None = None

        if preferred_ports is not None:
            if is_port_available(preferred_ports.dev_server_port, allow_reuse=True):
                available_dev_port = preferred_ports.dev_server_port
            if is_port_available(preferred_ports.frontend_port, allow_reuse=True):
                available_frontend_port = preferred_ports.frontend_port
            if is_port_available(preferred_ports.backend_port, allow_reuse=True):
                available_backend_port = preferred_ports.backend_port

        if available_dev_port is None:
            available_dev_port = find_available_port(
                DEV_SERVER_PORT_START, DEV_SERVER_PORT_END
            )
        if available_dev_port is None:
            console.print(
                f"[red]❌ No available ports for dev server in range {DEV_SERVER_PORT_START}-{DEV_SERVER_PORT_END}[/red]"
            )
            raise Exit(code=1)

        if available_frontend_port is None:
            available_frontend_port = find_available_port(
                FRONTEND_PORT_START, FRONTEND_PORT_END
            )
        if available_frontend_port is None:
            console.print(
                f"[red]❌ No available ports for frontend in range {FRONTEND_PORT_START}-{FRONTEND_PORT_END}[/red]"
            )
            raise Exit(code=1)

        if available_backend_port is None:
            available_backend_port = find_available_port(
                BACKEND_PORT_START, BACKEND_PORT_END
            )
        if available_backend_port is None:
            console.print(
                f"[red]❌ No available ports for backend in range {BACKEND_PORT_START}-{BACKEND_PORT_END}[/red]"
            )
            raise Exit(code=1)

        config = DevServerConfig(
            ports=PortsConfig(
                dev_server_port=available_dev_port,
                frontend_port=available_frontend_port,
                backend_port=available_backend_port,
            ),
            host=config.host,
            api_prefix=config.api_prefix,
            obo=config.obo,
            openapi=config.openapi,
            max_retries=config.max_retries,
            watch=config.watch,
        )

        console.print(
            f"[green]✓[/green] Found available ports - Dev: {config.dev_server_port}, Frontend: {config.frontend_port}, Backend: {config.backend_port}"
        )
        console.print()

        mode_msg = (
            "🚀 Starting development server in watch mode..."
            if config.watch
            else "🚀 Starting development server in detached mode..."
        )

        console.print(f"[bold chartreuse1]{mode_msg}[/bold chartreuse1]")
        console.print(
            f"[cyan]Dev Server:[/cyan] http://{config.host}:{config.dev_server_port}"
        )
        console.print(
            f"[cyan]  └─ Frontend:[/cyan] http://{config.host}:{config.frontend_port} (internal)"
        )
        console.print(
            f"[cyan]  └─ Backend:[/cyan] http://{config.host}:{config.backend_port} (internal)"
        )
        console.print(f"[cyan]  └─ API Prefix:[/cyan] {config.api_prefix}")
        console.print()

        # Start the dev server process
        popen_kwargs: dict[str, Any] = {}
        if os.name == "nt":
            popen_kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
        else:
            popen_kwargs["start_new_session"] = True

        dev_server_proc = subprocess.Popen(
            [
                "uv",
                "run",
                "apx",
                "dev",
                "_run_server",
                str(self.app_dir),
                str(config.dev_server_port),
                str(config.frontend_port),
                str(config.backend_port),
                config.host,
                config.api_prefix,
                str(config.obo).lower(),
                str(config.openapi).lower(),
                str(config.max_retries),
            ],
            cwd=self.app_dir,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **popen_kwargs,
        )

        # Persist dev server process metadata
        project_config = self.get_or_create_config()
        project_config.dev.dev_server_pid = dev_server_proc.pid
        project_config.dev.dev_server_port = config.dev_server_port
        project_config.dev.api_prefix = config.api_prefix
        write_project_config(self.project_json_path, project_config)

        console.print("[cyan]✓[/cyan] Dev server process started")
        console.print()

        # Wait for dev server to be ready
        max_wait = 10
        client = DevServerClient(port=config.dev_server_port, timeout=2.0)
        for _ in range(max_wait * 10):
            if client.is_running():
                break
            time.sleep(0.1)
        else:
            console.print(
                "[red]❌ Dev server did not become ready within timeout[/red]"
            )
            try:
                dev_server_proc.terminate()
                dev_server_proc.wait(timeout=2)
            except Exception:
                pass
            raise Exit(code=1)

        # Send start request to dev server
        try:
            request = ActionRequest.from_config(config)
            response = client.start(request)

            if response.status == "success":
                console.print(
                    "[bold green]✨ Development servers started successfully![/bold green]"
                )
            else:
                console.print(f"[yellow]⚠️  Warning: {response.message}[/yellow]")
        except Exception as e:
            console.print(
                f"[yellow]⚠️  Warning: Could not connect to dev server: {e}[/yellow]"
            )

        console.print()
        console.print(
            f"[bold]Open in browser:[/bold] http://{config.host}:{config.dev_server_port}"
        )
        if not config.watch:
            console.print(
                "[dim]Run 'apx dev status' to check status or 'apx dev stop' to stop the servers.[/dim]"
            )

    def status(self) -> None:
        """Check the status of development servers."""
        config = self.get_or_create_config()
        port = config.dev.dev_server_port

        if port is None or not self.is_running():
            console.print("[yellow]No development server found.[/yellow]")
            console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
            return

        client = DevServerClient(port=port)

        try:
            status_data = client.status()

            table = Table(
                title="Development Server Status",
                show_header=True,
                header_style="bold magenta",
            )
            table.add_column("Process", style="cyan", width=12)
            table.add_column("Status", justify="center")
            table.add_column("Port", justify="right", style="green")

            table.add_row(
                "Dev Server",
                "[green]●[/green] Running",
                str(status_data.dev_server_port),
            )

            if status_data.frontend_running:
                frontend_status = "[green]●[/green] Running"
            elif status_data.frontend_exit_code is not None:
                frontend_status = (
                    f"[red]●[/red] Exited ({status_data.frontend_exit_code})"
                )
            else:
                frontend_status = "[red]●[/red] Stopped"
            table.add_row(
                "Frontend",
                frontend_status,
                f"{status_data.frontend_port} (internal)",
            )

            if status_data.backend_running:
                backend_status = "[green]●[/green] Running"
            elif status_data.backend_exit_code is not None:
                backend_status = (
                    f"[red]●[/red] Exited ({status_data.backend_exit_code})"
                )
            else:
                backend_status = "[red]●[/red] Stopped"
            table.add_row(
                "Backend",
                backend_status,
                f"{status_data.backend_port} (internal)",
            )

            openapi_status = (
                "[green]●[/green] Running"
                if status_data.openapi_running
                else "[red]●[/red] Stopped"
            )
            table.add_row("OpenAPI", openapi_status, "-")

            console.print(table)
            console.print()
            console.print(f"[dim]API Prefix: {status_data.api_prefix}[/dim]")
            console.print(
                f"[dim]Open in browser: http://localhost:{status_data.dev_server_port}[/dim]"
            )
            console.print(
                "[dim]Use 'apx dev logs' to view logs or 'apx dev logs -f' to stream continuously.[/dim]"
            )
        except Exception as e:
            console.print(f"[yellow]⚠️  Could not connect to dev server: {e}[/yellow]")

    def stop(self) -> None:
        """Stop development server.

        Stops servers deterministically by targeting only processes we started.
        """
        config = self.get_or_create_config()
        port = config.dev.dev_server_port

        if port is None:
            console.print("[yellow]No development server found.[/yellow]")
            return

        console.print("[bold yellow]Stopping development server...[/bold yellow]")

        client = DevServerClient(port=port, timeout=15.0)

        try:
            if client.is_running():
                response = client.stop()
                if response.status == "success":
                    console.print("[green]✓[/green] Stopped all servers")
                else:
                    raise RuntimeError(response.message)
            else:
                raise RuntimeError("Dev server not responding")
        except Exception as e:
            console.print(
                f"[yellow]⚠️  Dev server API stop failed; falling back to process cleanup: {e}[/yellow]"
            )

            if config.dev.dev_server_pid is not None:
                tp = track_process(config.dev.dev_server_pid)
                if tp is not None:
                    stop_tracked_process(
                        tp,
                        name="dev-server",
                        sigterm_timeout=1.0,
                        sigkill_timeout=1.0,
                    )
                    wait_for_no_descendants(tp, timeout=3.0, poll=0.1)
                else:
                    try:
                        os.kill(config.dev.dev_server_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
            else:
                cleanup_dev_server_processes(self.app_dir, silent=True)

        # Wait for port to be free
        if port is not None:
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=port,
                timeout=5.0,
                poll=0.1,
            ):
                listeners = find_listeners_for_port(port)
                if listeners and len(listeners) > 0:
                    console.print(
                        f"[yellow]⚠️  Dev server port {port} still in use (PIDs: {listeners})[/yellow]"
                    )

        # Clear stored process metadata (token_id remains)
        config = self.get_or_create_config()
        config.dev.dev_server_pid = None
        config.dev.dev_server_port = None
        write_project_config(self.project_json_path, config)

        console.print()
        console.print(
            "[bold green]✨ Development server stopped successfully![/bold green]"
        )
        console.print("[dim]Token remains valid in keyring until expiration[/dim]")

    def restart(self) -> None:
        """Restart development servers with port stickiness."""
        if not self.is_running():
            console.print("[yellow]No development server found. Starting...[/yellow]")
            self.start()
            console.print(
                "[bold green]✨ Development servers started successfully![/bold green]"
            )
            return

        console.print("[bold yellow]🔄 Restarting development servers...[/bold yellow]")

        # Capture current port configuration before stopping
        project_config = self.get_or_create_config()
        old_dev_port = project_config.dev.dev_server_port

        self.stop()

        # Check if the dev server port is available for reuse
        if old_dev_port is not None:
            if is_port_available(old_dev_port, allow_reuse=True):
                console.print(
                    f"[green]✓[/green] Port {old_dev_port} is available for reuse"
                )
            else:
                console.print(
                    f"[yellow]⚠️  Port {old_dev_port} still in use, will find new port[/yellow]"
                )
                old_dev_port = None

        # Build preferred ports from old configuration
        preferred_ports: PortsConfig | None = None
        if old_dev_port is not None:
            preferred_ports = PortsConfig(dev_server_port=old_dev_port)

        self.start(preferred_ports=preferred_ports)

        console.print(
            "[bold green]✨ Development servers restarted successfully![/bold green]"
        )

    def stream_logs(
        self,
        duration_seconds: int | None = None,
        ui_only: bool = False,
        backend_only: bool = False,
        openapi_only: bool = False,
        app_only: bool = False,
        system_only: bool = False,
        raw_output: bool = False,
        follow: bool = False,
        timeout_seconds: int | None = None,
    ) -> None:
        """Stream logs from dev server using SSE.

        Args:
            duration_seconds: Show logs from last N seconds
            ui_only: Only show frontend logs
            backend_only: Only show backend logs
            openapi_only: Only show OpenAPI logs
            app_only: Only show application logs
            system_only: Only show system logs ([apx])
            raw_output: Show raw log output without prefix formatting
            follow: Continue streaming new logs (like tail -f)
            timeout_seconds: Stop streaming after N seconds
        """
        config = self.get_or_create_config()
        port = config.dev.dev_server_port

        if port is None or not self.is_running():
            console.print("[yellow]No development server found.[/yellow]")
            return

        channel_filter: Literal["app", "ui", "apx", "all"] = "all"
        component_filter: str | None = None
        include_system = False

        if system_only:
            channel_filter = "apx"
            include_system = True
        elif openapi_only:
            channel_filter = "apx"
            component_filter = "openapi"
            include_system = True
        elif ui_only:
            channel_filter = "ui"
        elif backend_only:
            channel_filter = "app"
        elif app_only:
            channel_filter = "app"
            component_filter = "backend"

        client = DevServerClient(port=port)

        log_count = 0
        server_disconnected = False

        try:
            with client.stream_logs(
                duration=duration_seconds,
                channel=channel_filter,
                component=component_filter,
                include_system=include_system,
            ) as log_stream:
                start_time = time.time()

                for item in log_stream:
                    if (
                        timeout_seconds
                        and (time.time() - start_time) >= timeout_seconds
                    ):
                        if follow:
                            console.print(
                                "\n[dim]Timeout reached, stopping stream.[/dim]"
                            )
                        break

                    if item == StreamEvent.SERVER_SHUTDOWN:
                        server_disconnected = True
                        break

                    if item == StreamEvent.BUFFERED_DONE:
                        if not follow:
                            break
                        continue

                    if not isinstance(item, LogEntry):
                        continue

                    print_log_entry(item, raw_output=raw_output)
                    log_count += 1

                # If we're following and the loop exits normally (without exception),
                # it means the server closed the connection
                if follow and not server_disconnected:
                    server_disconnected = True

        except KeyboardInterrupt:
            if follow:
                console.print("\n[dim]Stopped streaming logs.[/dim]")
        except Exception as e:
            console.print(f"\n[red]Error streaming logs: {e}[/red]")

        if server_disconnected:
            console.print("\n[dim]Development server stopped.[/dim]")
        elif not follow:
            if log_count > 0:
                console.print(f"\n[dim]Showed {log_count} log entries[/dim]")
            else:
                console.print("[dim]No logs found[/dim]")
