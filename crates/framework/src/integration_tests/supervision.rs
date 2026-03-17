//! Integration tests for the supervision shutdown logic.
//!
//! Verifies that `shutdown_workers` force-kills worker processes that linger
//! after acknowledging drain (the IPC Drained message).

use crate::supervision::ipc::channel::{accept, connect, listen};
use crate::supervision::ipc::protocol::IpcMessage;
use crate::supervision::supervisor::{WorkerHandle, shutdown_workers};
use std::time::Instant;
use tokio::process::Command;

/// Spawn a child process that ignores SIGTERM and hangs forever.
/// Only SIGKILL will terminate it.
fn spawn_unkillable() -> tokio::process::Child {
    Command::new("sh")
        .args(["-c", "trap '' TERM INT; sleep 300"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn test child process")
}

/// Create a connected UDS pair (supervisor side, worker side).
async fn uds_pair(
    path: &str,
) -> (
    crate::supervision::ipc::channel::WorkerChannel,
    crate::supervision::ipc::channel::WorkerChannel,
) {
    let listener = listen(path).expect("listen failed");
    let accept_task = tokio::spawn(async move { accept(&listener).await.expect("accept failed") });
    let worker_side = connect(path).await.expect("connect failed");
    let supervisor_side = accept_task.await.expect("accept task failed");
    (supervisor_side, worker_side)
}

/// After a worker sends `Drained` but refuses to exit (ignores SIGTERM),
/// `shutdown_workers` must force-kill it via SIGKILL.
#[tokio::test]
async fn shutdown_kills_workers_that_linger_after_drain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("drain.sock");
    let sock_str = sock.to_str().expect("path");

    let child = spawn_unkillable();
    let (sup_ch, mut worker_ch) = uds_pair(sock_str).await;

    // Worker-side task: respond to Drain with Drained, then hang (never exit).
    tokio::spawn(async move {
        let msg = worker_ch.recv().await.expect("recv Drain");
        assert!(
            matches!(msg, IpcMessage::Drain),
            "expected Drain, got {msg:?}"
        );
        worker_ch
            .send(&IpcMessage::Drained)
            .await
            .expect("send Drained");
        // Hold the channel open — do NOT drop or exit.
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    });

    let mut workers = vec![WorkerHandle::new_for_test(0, child, sup_ch)];

    let start = Instant::now();
    shutdown_workers(&mut workers).await;
    let elapsed = start.elapsed();

    // The process should be reaped after shutdown_workers returns.
    // try_wait returns Ok(Some(status)) if the child has already exited.
    let status = workers[0]
        .child
        .try_wait()
        .expect("try_wait should not error");
    assert!(
        status.is_some(),
        "worker process should have exited after shutdown"
    );

    // Shutdown should complete within a reasonable time (SIGKILL_TIMEOUT + margin).
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "shutdown took too long: {elapsed:?}"
    );
}

/// When workers drain and exit promptly, `shutdown_workers` returns quickly
/// without needing to SIGKILL.
#[tokio::test]
async fn shutdown_returns_quickly_when_workers_exit_after_drain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("fast.sock");
    let sock_str = sock.to_str().expect("path");

    // Use a process that will exit when its stdin is closed (cat).
    let mut child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn cat");

    // Drop stdin so `cat` will exit when it reads EOF.
    drop(child.stdin.take());

    let (sup_ch, mut worker_ch) = uds_pair(sock_str).await;

    tokio::spawn(async move {
        let msg = worker_ch.recv().await.expect("recv Drain");
        assert!(matches!(msg, IpcMessage::Drain));
        worker_ch
            .send(&IpcMessage::Drained)
            .await
            .expect("send Drained");
    });

    let mut workers = vec![WorkerHandle::new_for_test(0, child, sup_ch)];

    let start = Instant::now();
    shutdown_workers(&mut workers).await;
    let elapsed = start.elapsed();

    // Should complete well under the SIGKILL_TIMEOUT since the process exits fast.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "fast shutdown took too long: {elapsed:?}"
    );
}
