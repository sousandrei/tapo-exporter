mod config;
mod http;
mod metrics;
mod sensor;
mod shutdown;

use std::time::Instant;

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
        let _ = signal_shutdown.send(true);
    });

    let hub = match sensor::connect(config).await {
        Ok(hub) => {
            tracing::info!("connected to Tapo hub");
            hub
        }
        Err(error) => {
            tracing::error!(%error, "failed to connect to Tapo hub");
            signal_task.abort();
            if let Err(join_error) = signal_task.await {
                if !join_error.is_cancelled() {
                    tracing::error!(%join_error, "shutdown signal task failed");
                }
            }
            return Err(error.into());
        }
    };

    let upkeep_task = metrics::spawn_upkeep(prometheus_handle, shutdown_rx.clone());
    let mut polling_task = tokio::spawn(sensor::run(hub, shutdown_rx.clone()));
    let shutdown_status = shutdown_rx.clone();
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown::wait(shutdown_rx))
            .await
    });
    let mut fatal_server_error: Option<Box<dyn std::error::Error>> = None;
    let mut server_task_finished = false;
    let mut polling_task_finished = false;

    tokio::select! {
        result = &mut server_task => {
            server_task_finished = true;
            match result {
                Ok(Ok(())) => tracing::info!("HTTP server stopped"),
                Ok(Err(error)) => {
                    tracing::error!(%error, "HTTP server failed");
                    fatal_server_error = Some(Box::new(error));
                    let _ = shutdown_tx.send(true);
                }
                Err(error) => {
                    tracing::error!(%error, "HTTP server task failed");
                    fatal_server_error = Some(Box::new(error));
                    let _ = shutdown_tx.send(true);
                }
            }
        }
        result = &mut polling_task => {
            polling_task_finished = true;
            match result {
                Ok(()) if *shutdown_status.borrow() => {
                    tracing::info!("Tapo polling task stopped");
                }
                Ok(()) => tracing::error!("Tapo polling task exited unexpectedly"),
                Err(error) => {
                    tracing::error!(%error, "Tapo polling task failed");
                    let _ = shutdown_tx.send(true);
                }
            }
        }
    }

    let _ = shutdown_tx.send(true);
    signal_task.abort();
    if let Err(join_error) = signal_task.await {
        if !join_error.is_cancelled() {
            tracing::error!(%join_error, "shutdown signal task failed");
        }
    }
    if !server_task_finished {
        match server_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(%error, "HTTP server failed during shutdown"),
            Err(join_error) => tracing::error!(%join_error, "HTTP server task failed to join"),
        }
    }
    if !polling_task_finished {
        match polling_task.await {
            Ok(()) => {}
            Err(join_error) => tracing::error!(%join_error, "Tapo polling task failed to join"),
        }
    }
    match upkeep_task.await {
        Ok(()) => {}
        Err(join_error) => tracing::error!(%join_error, "Prometheus upkeep task failed to join"),
    }

    if let Some(error) = fatal_server_error {
        return Err(error);
    }

    Ok(())
}
