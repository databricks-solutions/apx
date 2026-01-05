"""Cross-platform process tracking and robust stop/restart helpers for apx dev.

Design goals:
- Only stop processes we started (tracked by pid + create_time).
- Use SIGTERM -> SIGKILL sequence for reliable termination (no SIGINT).
- Target process groups to catch child processes (vite/esbuild) that outlive parents (bun).
- Verify children are gone and ports are released.
- Work on POSIX + Windows (best-effort graceful on Windows).
"""

from __future__ import annotations
import os
import signal
import time
from pathlib import Path
from typing import Callable

import psutil

from apx.models import TrackedProcess
from apx.cli.dev.logging import DevLogComponent, get_logger

logger = get_logger(DevLogComponent.PROCESS_CONTROL)


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


def kill_process_group(
    pgid: int,
    *,
    sigterm_timeout: float = 1.0,
    sigkill_timeout: float = 1.0,
) -> bool:
    """Aggressively kill all processes in a process group with SIGTERM then SIGKILL.

    This is the primary method for stopping frontend processes (vite/esbuild) that
    may outlive their parent (bun). Unlike the old approach that relied on the
    tracked parent being alive, this targets the process group directly.

    Args:
        pgid: Process group ID to kill
        sigterm_timeout: Seconds to wait after SIGTERM before escalating to SIGKILL
        sigkill_timeout: Seconds to wait after SIGKILL for processes to exit

    Returns:
        True if process group is empty after termination, False otherwise
    """
    if os.name == "nt":
        # Windows doesn't have process groups in the POSIX sense
        return True

    logger.debug(f"Killing process group pgid={pgid}")

    # Step 1: Get current members before sending any signals
    members_before = _list_pgid_members(pgid)
    if not members_before:
        logger.debug(f"Process group pgid={pgid} already empty")
        return True

    logger.debug(
        f"Process group pgid={pgid} has {len(members_before)} members: {members_before}"
    )

    # Step 2: Send SIGTERM to entire process group
    try:
        _send_posix_signal_group(pgid, signal.SIGTERM)
        logger.debug(f"Sent SIGTERM to pgid={pgid}")
    except ProcessLookupError:
        # Group already gone
        return True
    except Exception as e:
        logger.debug(f"Failed to send SIGTERM to pgid={pgid}: {e}")

    # Step 3: Wait for processes to exit gracefully
    if _wait_for_pgid_empty(pgid, sigterm_timeout, poll=0.1):
        logger.debug(f"Process group pgid={pgid} exited after SIGTERM")
        return True

    # Step 4: Send SIGKILL to entire process group
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

    # Step 5: Wait for SIGKILL to take effect
    if _wait_for_pgid_empty(pgid, sigkill_timeout, poll=0.1):
        logger.debug(f"Process group pgid={pgid} exited after SIGKILL")
        return True

    # Step 6: Kill any remaining stragglers individually (belt and suspenders)
    stragglers = _list_pgid_members(pgid)
    if stragglers:
        logger.debug(f"Killing {len(stragglers)} stragglers individually: {stragglers}")
        for pid in stragglers:
            try:
                os.kill(pid, signal.SIGKILL)
            except Exception:
                pass
        # Brief wait for stragglers
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
    sigterm_timeout: float = 1.0,
    sigkill_timeout: float = 1.0,
) -> None:
    """Stop a tracked process and its children.

    Behavior:
    - POSIX: signal process group with SIGTERM -> SIGKILL (no SIGINT).
      This is aggressive but reliable for stopping vite/esbuild which may
      outlive their bun parent process.
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

    # POSIX: prefer process group for reliable shutdown of vite/esbuild
    pgid = tp.pgid or (_get_pgid_safe(tp.pid) if tp.pid is not None else None)

    logger.debug(f"Stopping {name} pid={tp.pid} pgid={pgid}")

    if pgid is not None:
        # Use aggressive process group killing (SIGTERM -> SIGKILL)
        # This works even if the original bun process has already exited
        kill_process_group(
            pgid,
            sigterm_timeout=sigterm_timeout,
            sigkill_timeout=sigkill_timeout,
        )

    # Fallback: also try to terminate the tracked process tree directly
    # This handles edge cases where:
    # 1. PGID is None
    # 2. Process didn't inherit the PGID properly
    # 3. Children somehow escaped the process group
    proc = validate_tracked(tp)
    if proc is not None:
        try:
            # Get children before we terminate the parent
            children = proc.children(recursive=True)
        except Exception:
            children = []

        # Terminate parent
        try:
            proc.terminate()
        except Exception:
            pass

        # Terminate children
        for child in children:
            try:
                child.terminate()
            except Exception:
                pass

        # Wait briefly
        all_procs = [proc] + children
        _, alive = psutil.wait_procs(all_procs, timeout=sigterm_timeout)

        # Kill anything still alive
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
        logger.info(f"Cleaned up {killed} dev-server process(es) for {app_dir_str}")
    return killed
