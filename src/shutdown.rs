use std::future::Future;

use tokio::sync::watch;

pub(crate) async fn wait(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    // A closed channel means the sender is gone, which is also a shutdown
    // condition during teardown.
    let _ = shutdown.changed().await;
}

pub(crate) async fn changed(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    // A closed channel means the sender is gone, which is also a shutdown
    // condition during teardown.
    let _ = shutdown.changed().await;
}

pub(crate) async fn wait_for_startup<F, T, E>(
    connection: F,
    shutdown: watch::Receiver<bool>,
) -> Result<Option<T>, E>
where
    F: Future<Output = Result<T, E>>,
{
    tokio::select! {
        result = connection => result.map(Some),
        _ = wait(shutdown) => Ok(None),
    }
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

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, future::pending};

    use super::*;

    #[tokio::test]
    async fn startup_wait_returns_when_shutdown_is_requested() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let connection = wait_for_startup(pending::<Result<(), Infallible>>(), shutdown_rx);

        shutdown_tx
            .send(true)
            .expect("shutdown receiver disappeared");

        assert_eq!(connection.await.expect("startup wait failed"), None);
    }

    #[tokio::test]
    async fn startup_wait_returns_connection_result() {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let connection = wait_for_startup(async { Ok::<_, Infallible>(()) }, shutdown_rx);

        assert_eq!(connection.await.expect("startup wait failed"), Some(()));
    }
}
