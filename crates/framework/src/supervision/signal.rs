//! Shutdown signal handling shared by supervisor and worker.

/// Wait for a SIGTERM or SIGINT signal (graceful shutdown).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.unwrap_or_else(|e| {
            tracing::error!(
                name: "apx.signal.ctrl_c_handler_error",
                "ctrl_c signal handler failed: {e}"
            );
        });
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
