mod config;
mod http;
mod metrics;
mod sensor;
mod shutdown;

use std::{io, time::Instant};

use tokio::sync::watch;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let boot_time = Instant::now();
    let config_result = config::Config::from_env();
    let env_filter = match &config_result {
        Ok(config) => EnvFilter::new(&config.rust_log),
        Err(_) => EnvFilter::new("info"),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("starting tapo exporter");

    let config = config_result.map_err(|error| {
        tracing::error!(%error, "invalid configuration");
        error
    })?;
    let prometheus_handle = metrics::install_recorder()?;
    metrics::describe_metrics();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let app_state = http::AppState::new(prometheus_handle.clone(), boot_time);
    let app = http::build_router(app_state);
    let listener = match tokio::net::TcpListener::bind("0.0.0.0:3000").await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, address = "0.0.0.0:3000", "failed to bind HTTP server");
            return Err(error.into());
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            tracing::error!(%error, "failed to determine HTTP server address");
            return Err(error.into());
        }
    };
    tracing::info!(%address, "HTTP server listening");

    let signal_shutdown = shutdown_tx.clone();
    let signal_task = tokio::spawn(async move {
        shutdown::signal().await;
        tracing::info!("shutdown signal received");
        request_shutdown(&signal_shutdown);
    });

    let hub = match shutdown::wait_for_startup(sensor::connect(config), shutdown_rx.clone()).await {
        Ok(Some(hub)) => {
            tracing::info!("connected to Tapo hub");
            hub
        }
        Ok(None) => {
            tracing::info!("startup interrupted by shutdown");
            signal_task.abort();
            join_signal_task(signal_task).await;
            return Ok(());
        }
        Err(error) => {
            tracing::error!(%error, "failed to connect to Tapo hub");
            signal_task.abort();
            join_signal_task(signal_task).await;
            return Err(error.into());
        }
    };

    let mut upkeep_task = metrics::spawn_upkeep(prometheus_handle, shutdown_rx.clone());
    let mut polling_task = tokio::spawn(sensor::run(hub, shutdown_rx.clone()));
    let shutdown_status = shutdown_rx.clone();
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown::wait(shutdown_rx))
            .await
    });
    let mut fatal_error: Option<Box<dyn std::error::Error>> = None;
    let mut server_task_finished = false;
    let mut polling_task_finished = false;
    let mut upkeep_task_finished = false;

    tokio::select! {
        result = &mut server_task => {
            server_task_finished = true;
            match result {
                Ok(Ok(())) if *shutdown_status.borrow() => tracing::info!("HTTP server stopped"),
                Ok(Ok(())) => {
                    let error = unexpected_task_exit("HTTP server");
                    tracing::error!(%error, "HTTP server stopped unexpectedly");
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
                Ok(Err(error)) => {
                    tracing::error!(%error, "HTTP server failed");
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
                Err(error) => {
                    tracing::error!(%error, "HTTP server task failed");
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
            }
        }
        result = &mut polling_task => {
            polling_task_finished = true;
            match result {
                Ok(()) if *shutdown_status.borrow() => {
                    tracing::info!("Tapo polling task stopped");
                }
                Ok(()) => {
                    let error = unexpected_task_exit("Tapo polling task");
                    tracing::error!(%error);
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
                Err(error) => {
                    tracing::error!(%error, "Tapo polling task failed");
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
            }
        }
        result = &mut upkeep_task => {
            upkeep_task_finished = true;
            match result {
                Ok(()) if *shutdown_status.borrow() => {
                    tracing::info!("Prometheus upkeep task stopped");
                }
                Ok(()) => {
                    let error = unexpected_task_exit("Prometheus upkeep task");
                    tracing::error!(%error);
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
                Err(error) => {
                    tracing::error!(%error, "Prometheus upkeep task failed");
                    fatal_error = Some(Box::new(error));
                    request_shutdown(&shutdown_tx);
                }
            }
        }
    }

    request_shutdown(&shutdown_tx);
    signal_task.abort();
    join_signal_task(signal_task).await;
    if !server_task_finished {
        match server_task.await {
            Ok(Ok(())) => tracing::debug!("HTTP server task joined during shutdown"),
            Ok(Err(error)) => {
                tracing::error!(%error, "HTTP server failed during shutdown");
                if fatal_error.is_none() {
                    fatal_error = Some(Box::new(error));
                }
            }
            Err(join_error) => {
                tracing::error!(%join_error, "HTTP server task failed to join");
                if fatal_error.is_none() {
                    fatal_error = Some(Box::new(join_error));
                }
            }
        }
    }
    if !polling_task_finished {
        match polling_task.await {
            Ok(()) => tracing::debug!("Tapo polling task joined during shutdown"),
            Err(join_error) => {
                tracing::error!(%join_error, "Tapo polling task failed to join");
                if fatal_error.is_none() {
                    fatal_error = Some(Box::new(join_error));
                }
            }
        }
    }
    if !upkeep_task_finished {
        match upkeep_task.await {
            Ok(()) => tracing::debug!("Prometheus upkeep task joined during shutdown"),
            Err(join_error) => {
                tracing::error!(%join_error, "Prometheus upkeep task failed to join");
                if fatal_error.is_none() {
                    fatal_error = Some(Box::new(join_error));
                }
            }
        }
    }

    if let Some(error) = fatal_error {
        return Err(error);
    }

    Ok(())
}

fn unexpected_task_exit(task_name: &str) -> io::Error {
    io::Error::other(format!("{task_name} exited unexpectedly"))
}

fn request_shutdown(shutdown: &watch::Sender<bool>) {
    if shutdown.send(true).is_err() {
        tracing::debug!("shutdown channel was already closed");
    }
}

async fn join_signal_task(signal_task: tokio::task::JoinHandle<()>) {
    match signal_task.await {
        Ok(()) => tracing::debug!("shutdown signal task joined"),
        Err(join_error) if join_error.is_cancelled() => {
            tracing::debug!("shutdown signal task cancelled during cleanup");
        }
        Err(join_error) => {
            tracing::error!(%join_error, "shutdown signal task failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unexpected_task_exit;

    #[test]
    fn unexpected_task_exit_preserves_task_name() {
        let error = unexpected_task_exit("Tapo polling task");

        assert_eq!(error.to_string(), "Tapo polling task exited unexpectedly");
    }
}
