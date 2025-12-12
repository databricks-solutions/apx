"""Cross-platform process tracking and robust stop/restart helpers for apx dev.

Design goals:
- Only stop processes we started (tracked by pid + create_time).
- Prefer graceful shutdown (SIGINT first), escalate deterministically.
- Verify children are gone and ports are released.
- Work on POSIX + Windows (best-effort graceful on Windows).
"""

from __future__ import annotations

import logging
import os
import signal
import time
from pathlib import Path
from typing import Callable, ClassVar

import psutil
from pydantic import BaseModel, ConfigDict


logger = logging.getLogger("apx.process_control")


class TrackedProcess(BaseModel):
    """A process we started and are allowed to manage."""

    pid: int | None = None
    create_time: float | None = None
    pgid: int | None = None

    model_config: ClassVar[ConfigDict] = ConfigDict(frozen=True)


def _get_pgid_safe(pid: int) -> int | None:
    # Windows doesn't have pgid.
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
    proc = validate_tracked(tp)
    if proc is None:
        return []
    try:
        return proc.children(recursive=True)
    except Exception:
        return []


def _send_posix_signal_group(pgid: int, sig: signal.Signals) -> None:
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
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not _list_pgid_members(pgid):
            return True
        time.sleep(poll)
    return not _list_pgid_members(pgid)


def _wait_for_exit(proc: psutil.Process, timeout: float) -> bool:
    try:
        proc.wait(timeout=timeout)
        return True
    except psutil.TimeoutExpired:
        return False
    except Exception:
        return True


def _terminate_tree_posix(root: psutil.Process, timeout: float) -> None:
    """Terminate a process tree on POSIX (best-effort).

    This is used as a fallback when process-group signaling doesn't fully stop a tree
    (e.g. stubborn child processes that don't respond to SIGINT/SIGTERM).
    """
    children: list[psutil.Process]
    try:
        children = root.children(recursive=True)
    except Exception:
        children = []

    # Try terminate children first (gives root a chance to exit cleanly).
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

    # Try terminate children first (gives root a chance to exit cleanly).
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
    name: str,
    sigint_timeout: float = 1.0,
    sigterm_timeout: float = 1.5,
    sigkill_timeout: float = 1.0,
) -> None:
    """Stop a tracked process and its children.

    Behavior:
    - POSIX: signal process group (SIGINT -> SIGTERM -> SIGKILL).
    - Windows: best-effort CTRL_BREAK_EVENT, then terminate/kill process tree.
    """
    if os.name == "nt":
        proc = validate_tracked(tp)
        if proc is None or tp.pid is None:
            return
        logger.debug(f"Stopping {name} pid={tp.pid}")
        # Best-effort graceful: CTRL_BREAK_EVENT requires the process to be in a new group.
        # It can also fail if the child isn't attached to a console; in that case we fall back.
        try:
            os.kill(tp.pid, signal.CTRL_BREAK_EVENT)  # type: ignore[attr-defined]
        except Exception:
            pass
        time.sleep(0.1)
        _terminate_tree_windows(proc, timeout=sigterm_timeout + sigkill_timeout)
        return

    # POSIX: prefer process group.
    pgid = tp.pgid or (_get_pgid_safe(tp.pid) if tp.pid is not None else None)
    if pgid is None:
        # Fallback: signal just the process (requires pid + create_time).
        proc = validate_tracked(tp)
        if proc is None:
            return
        try:
            proc.send_signal(signal.SIGINT)
        except Exception:
            pass
        if _wait_for_exit(proc, sigint_timeout):
            return
        try:
            proc.terminate()
        except Exception:
            pass
        if _wait_for_exit(proc, sigterm_timeout):
            return
        try:
            proc.kill()
        except Exception:
            pass
        _wait_for_exit(proc, sigkill_timeout)
        return

    # Group shutdown (works even if the original pid has already exited).
    try:
        _send_posix_signal_group(pgid, signal.SIGINT)
    except Exception:
        pass
    if _wait_for_pgid_empty(pgid, sigint_timeout):
        # Even if the pgid looks empty, be defensive: attempt to terminate the tree
        # for the tracked PID (if still valid) to avoid orphaned children.
        proc = validate_tracked(tp)
        if proc is not None:
            _terminate_tree_posix(proc, timeout=max(0.2, sigterm_timeout))
        return

    try:
        _send_posix_signal_group(pgid, signal.SIGTERM)
    except Exception:
        pass
    if _wait_for_pgid_empty(pgid, sigterm_timeout):
        proc = validate_tracked(tp)
        if proc is not None:
            _terminate_tree_posix(proc, timeout=max(0.2, sigterm_timeout))
        return

    try:
        _send_posix_signal_group(pgid, signal.SIGKILL)
    except Exception:
        pass
    _wait_for_pgid_empty(pgid, sigkill_timeout)

    # Last resort: if we still have a valid root process, kill its tree explicitly.
    proc = validate_tracked(tp)
    if proc is not None:
        _terminate_tree_posix(proc, timeout=max(0.2, sigkill_timeout))


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
    """Filter PIDs down to those that look like they're part of this app's dev frontend.

    Heuristics (any match qualifies):
    - Same process group id (pgid) if expected_pgid is provided
    - Process cwd equals app_dir (common for vite/node launched by bun in that dir)
    - Command line contains app_dir path (best-effort)
    """
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
    # Best-effort: if we know a pgid, that is the most reliable for bun->node handoff.
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
        # If proc is a zombie on POSIX, treat it as done.
        try:
            if proc.status() == psutil.STATUS_ZOMBIE:
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
        # Fall back below.
        access_denied = True

    # Fallback: on some platforms (notably macOS) net_connections may not reliably
    # attribute PIDs, or may require elevated privileges. For same-user processes,
    # iterating processes and checking per-process connections often works better.
    if not pids and access_denied:
        for proc in psutil.process_iter(["pid"]):
            try:
                pid = int(proc.pid)
                try:
                    conns = proc.net_connections(kind="inet")  # type: ignore[attr-defined]
                except Exception:
                    # Older psutil versions may not expose Process.net_connections.
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


def wait_for_port_free(
    *,
    is_port_available_fn: Callable[[int], bool],
    port: int,
    timeout: float = 10.0,
    poll: float = 0.1,
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if is_port_available_fn(port):
                return True
        except Exception:
            # If the check fails, be conservative and keep waiting.
            pass
        time.sleep(poll)
    return False


def cleanup_dev_server_processes(app_dir: Path, silent: bool = False) -> int:
    """Last-resort: stop dev server processes for an app_dir by cmdline matching.

    Intentionally narrow: only matches `apx dev _run_server <app_dir>`.
    """
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
        logging.getLogger("apx.process_control").info(
            f"Cleaned up {killed} dev-server process(es) for {app_dir_str}"
        )
    return killed
