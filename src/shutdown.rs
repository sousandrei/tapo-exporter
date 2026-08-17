use tokio::sync::watch;

pub(crate) async fn wait(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    let _ = shutdown.changed().await;
}

pub(crate) async fn changed(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    let _ = shutdown.changed().await;
}

pub(crate) async fn signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => Some(signal),
                Err(error) => {
                    tracing::error!(%error, "failed to install SIGTERM handler");
                    None
                }
            };
        let ctrl_c = tokio::signal::ctrl_c();

        if let Some(terminate) = &mut terminate {
            tokio::select! {
                result = ctrl_c => {
                    if let Err(error) = result {
                        tracing::error!(%error, "failed to listen for Ctrl-C");
                    }
                }
                _ = terminate.recv() => {}
            }
        } else if let Err(error) = ctrl_c.await {
            tracing::error!(%error, "failed to listen for Ctrl-C");
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to listen for Ctrl-C");
    }
}
