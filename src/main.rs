mod config;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use metrics_exporter_prometheus::PrometheusBuilder;
use tapo::responses::ChildDeviceHubResult::T31X;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const METRICS_LOG_INTERVAL: Duration = Duration::from_secs(60 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_SUCCESS_LOG_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct AppState {
    prometheus_handle: metrics_exporter_prometheus::PrometheusHandle,
    next_metrics_log: Arc<Mutex<Instant>>,
}

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

    let prometheus_handle = PrometheusBuilder::new()
        .idle_timeout(
            metrics_util::MetricKindMask::COUNTER | metrics_util::MetricKindMask::HISTOGRAM,
            Some(Duration::from_secs(10)),
        )
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    metrics::describe_gauge!("room_temperature", "Temperature in the room");
    metrics::describe_gauge!("room_humidity", "Humidity in the room");

    let upkeep_handle = prometheus_handle.clone();
    let app_state = AppState {
        prometheus_handle,
        next_metrics_log: Arc::new(Mutex::new(boot_time + METRICS_LOG_INTERVAL)),
    };
    let app = build_router(app_state);
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
        shutdown_signal().await;
        tracing::info!("shutdown signal received");
        let _ = signal_shutdown.send(true);
    });

    tracing::info!("connecting to Tapo hub");
    let hub = match tapo::ApiClient::new(config.tapo_username, config.tapo_password)
        .h100(config.tapo_hub_ip)
        .await
    {
        Ok(hub) => {
            tracing::info!("connected to Tapo hub");
            hub
        }
        Err(error) => {
            tracing::error!(%error, "failed to connect to Tapo hub");
            signal_task.abort();
            let _ = signal_task.await;
            return Err(error.into());
        }
    };

    let mut upkeep_shutdown = shutdown_rx.clone();
    let upkeep_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => upkeep_handle.run_upkeep(),
                _ = shutdown_changed(&mut upkeep_shutdown) => {
                    tracing::info!("Prometheus upkeep task stopped");
                    break;
                }
            }
        }
    });

    let mut polling_task = tokio::spawn(poll_tapo_devices(hub, shutdown_rx.clone()));
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx))
            .await
    });
    let mut fatal_server_error: Option<Box<dyn std::error::Error>> = None;

    tokio::select! {
        result = &mut server_task => {
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
            match result {
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
    let _ = signal_task.await;
    let _ = server_task.await;
    let _ = polling_task.await;
    let _ = upkeep_task.await;

    if let Some(error) = fatal_server_error {
        return Err(error);
    }

    Ok(())
}

async fn poll_tapo_devices(hub: tapo::HubHandler, mut shutdown: watch::Receiver<bool>) {
    let mut last_success_log = Instant::now() - POLL_SUCCESS_LOG_INTERVAL;

    loop {
        let devices = match tokio::select! {
            result = hub.get_child_device_list() => result,
            _ = shutdown_changed(&mut shutdown) => {
                tracing::info!("Tapo polling task stopped");
                return;
            }
        } {
            Ok(devices) => devices,
            Err(error) => {
                tracing::error!(%error, "failed to poll Tapo hub");
                tokio::select! {
                    _ = sleep(POLL_INTERVAL) => {}
                    _ = shutdown_changed(&mut shutdown) => {
                        tracing::info!("Tapo polling task stopped");
                        return;
                    }
                }
                continue;
            }
        };

        let mut updated_devices = 0;
        for device in devices {
            if let T31X(device) = device {
                metrics::gauge!("room_temperature", "name" => device.nickname.clone())
                    .set(device.current_temperature as f64);

                metrics::gauge!("room_humidity", "name" => device.nickname.clone())
                    .set(device.current_humidity as f64);
                updated_devices += 1;
            }
        }

        if last_success_log.elapsed() >= POLL_SUCCESS_LOG_INTERVAL {
            tracing::info!(updated_devices, "updated Tapo metrics");
            last_success_log = Instant::now();
        }

        tokio::select! {
            _ = sleep(POLL_INTERVAL) => {}
            _ = shutdown_changed(&mut shutdown) => {
                tracing::info!("Tapo polling task stopped");
                return;
            }
        }
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    let _ = shutdown.changed().await;
}

async fn shutdown_changed(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }

    let _ = shutdown.changed().await;
}

async fn shutdown_signal() {
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

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.prometheus_handle.render(),
    )
}

async fn healthz_handler() -> StatusCode {
    StatusCode::OK
}

async fn not_found_handler() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn request_logging(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status();
    let latency = started.elapsed();

    if should_log_request(
        &method,
        &path,
        status,
        Instant::now(),
        &mut state
            .next_metrics_log
            .lock()
            .expect("metrics log lock poisoned"),
    ) {
        if status == StatusCode::OK {
            tracing::info!(
                %method,
                %path,
                %status,
                latency_ms = latency.as_millis(),
                "HTTP request completed"
            );
        } else {
            tracing::warn!(
                %method,
                %path,
                %status,
                latency_ms = latency.as_millis(),
                "HTTP request failed"
            );
        }
    }

    response
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .fallback(not_found_handler)
        .layer(from_fn_with_state(state.clone(), request_logging))
        .with_state(state)
}

fn should_log_request(
    method: &Method,
    path: &str,
    status: StatusCode,
    now: Instant,
    next_metrics_log: &mut Instant,
) -> bool {
    if status != StatusCode::OK {
        return true;
    }

    if *method == Method::GET && path == "/metrics" && now >= *next_metrics_log {
        while now >= *next_metrics_log {
            *next_metrics_log += METRICS_LOG_INTERVAL;
        }
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::to_bytes, body::Body, http::Request};
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        let recorder = PrometheusBuilder::new().build_recorder();
        AppState {
            prometheus_handle: recorder.handle(),
            next_metrics_log: Arc::new(Mutex::new(Instant::now() + METRICS_LOG_INTERVAL)),
        }
    }

    #[test]
    fn successful_metrics_request_before_one_hour_is_suppressed() {
        let boot = Instant::now();
        let mut next_log = boot + METRICS_LOG_INTERVAL;

        assert!(!should_log_request(
            &Method::GET,
            "/metrics",
            StatusCode::OK,
            boot + Duration::from_secs(30 * 60),
            &mut next_log,
        ));
        assert_eq!(next_log, boot + METRICS_LOG_INTERVAL);
    }

    #[test]
    fn successful_metrics_request_at_one_hour_is_logged_and_advances_window() {
        let boot = Instant::now();
        let mut next_log = boot + METRICS_LOG_INTERVAL;

        assert!(should_log_request(
            &Method::GET,
            "/metrics",
            StatusCode::OK,
            boot + METRICS_LOG_INTERVAL,
            &mut next_log,
        ));
        assert_eq!(next_log, boot + 2 * METRICS_LOG_INTERVAL);
    }

    #[test]
    fn successful_metrics_request_after_multiple_windows_keeps_boot_schedule() {
        let boot = Instant::now();
        let mut next_log = boot + METRICS_LOG_INTERVAL;

        assert!(should_log_request(
            &Method::GET,
            "/metrics",
            StatusCode::OK,
            boot + 2 * METRICS_LOG_INTERVAL + Duration::from_secs(30 * 60),
            &mut next_log,
        ));
        assert_eq!(next_log, boot + 3 * METRICS_LOG_INTERVAL);
    }

    #[test]
    fn non_200_requests_are_always_logged_without_advancing_metrics_window() {
        let boot = Instant::now();
        let mut next_log = boot + METRICS_LOG_INTERVAL;

        assert!(should_log_request(
            &Method::GET,
            "/metrics",
            StatusCode::INTERNAL_SERVER_ERROR,
            boot + Duration::from_secs(1),
            &mut next_log,
        ));
        assert!(should_log_request(
            &Method::GET,
            "/unknown",
            StatusCode::NOT_FOUND,
            boot + Duration::from_secs(1),
            &mut next_log,
        ));
        assert_eq!(next_log, boot + METRICS_LOG_INTERVAL);
    }

    #[test]
    fn concurrent_successful_metrics_requests_only_log_once_per_window() {
        let boot = Instant::now();
        let next_log = Arc::new(Mutex::new(boot + METRICS_LOG_INTERVAL));
        let mut threads = Vec::new();

        for _ in 0..16 {
            let next_log = Arc::clone(&next_log);
            threads.push(std::thread::spawn(move || {
                let mut next_log = next_log.lock().expect("test lock poisoned");
                should_log_request(
                    &Method::GET,
                    "/metrics",
                    StatusCode::OK,
                    boot + METRICS_LOG_INTERVAL,
                    &mut next_log,
                )
            }));
        }

        let logged = threads
            .into_iter()
            .map(|thread| thread.join().expect("test thread panicked"))
            .filter(|logged| *logged)
            .count();
        assert_eq!(logged, 1);
    }

    #[tokio::test]
    async fn metrics_route_returns_prometheus_exposition() {
        let recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            metrics::describe_gauge!("room_temperature", "Temperature in the room");
            metrics::gauge!("room_temperature", "name" => "test-room").set(21.5);
        });
        let response = build_router(AppState {
            prometheus_handle: recorder.handle(),
            next_metrics_log: Arc::new(Mutex::new(Instant::now() + METRICS_LOG_INTERVAL)),
        })
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .expect("failed to build request"),
        )
        .await
        .expect("metrics request failed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("missing content type"),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("failed to read metrics body");
        let body = String::from_utf8(body.into()).expect("metrics body was not UTF-8");
        assert!(body.contains("room_temperature"));
        assert!(body.contains("name=\"test-room\""));
        assert!(body.contains("21.5"));
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let response = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("failed to build request"),
            )
            .await
            .expect("health request failed");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_route_returns_not_found() {
        let response = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/unknown")
                    .body(Body::empty())
                    .expect("failed to build request"),
            )
            .await
            .expect("unknown route request failed");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
