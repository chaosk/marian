//! Stopping cleanly on SIGTERM or Ctrl-C: Kubernetes sends SIGTERM on every
//! deploy and waits for the process to exit before it kills it.

use std::sync::LazyLock;

use tokio::sync::watch;

static STOPPING: LazyLock<watch::Sender<bool>> = LazyLock::new(|| watch::channel(false).0);

/// Asks everything that watches for it to finish up.
pub fn begin() {
    STOPPING.send_replace(true);
}

/// Whether shutdown has begun.
pub fn requested() -> bool {
    *STOPPING.borrow()
}

/// Returns once shutdown has begun.
pub async fn wait() {
    let mut stopping = STOPPING.subscribe();
    let _ = stopping.wait_for(|stopping| *stopping).await;
}

/// Shutdown's state, for streams that end with it.
pub fn subscribe() -> watch::Receiver<bool> {
    STOPPING.subscribe()
}

/// Begins shutdown on the first SIGTERM or Ctrl-C.
pub async fn on_signal() {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!("can't listen for Ctrl-C: {err}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(err) => {
                tracing::warn!("can't listen for SIGTERM: {err}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => tracing::info!("Ctrl-C: shutting down"),
        _ = terminate => tracing::info!("SIGTERM: shutting down"),
    }
    begin();
}
