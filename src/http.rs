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
use metrics_exporter_prometheus::PrometheusHandle;

#[cfg(test)]
use metrics_exporter_prometheus::PrometheusBuilder;

const METRICS_LOG_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub(crate) struct AppState {
    prometheus_handle: PrometheusHandle,
    next_metrics_log: Arc<Mutex<Instant>>,
}

impl AppState {
    pub(crate) fn new(prometheus_handle: PrometheusHandle, boot_time: Instant) -> Self {
        Self {
            prometheus_handle,
            next_metrics_log: Arc::new(Mutex::new(boot_time + METRICS_LOG_INTERVAL)),
        }
    }
}

pub(crate) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .fallback(not_found_handler)
        .layer(from_fn_with_state(state.clone(), request_logging))
        .with_state(state)
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

    let mut next_metrics_log = match state.next_metrics_log.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("metrics log lock was poisoned; recovering its state");
            poisoned.into_inner()
        }
    };

    if should_log_request(
        &method,
        &path,
        status,
        Instant::now(),
        &mut next_metrics_log,
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
    use axum::{body::to_bytes, body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        let recorder = PrometheusBuilder::new().build_recorder();
        AppState::new(recorder.handle(), Instant::now())
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

    #[test]
    fn poisoned_metrics_log_lock_can_be_recovered() {
        let next_log = Mutex::new(Instant::now() + METRICS_LOG_INTERVAL);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = next_log.lock().expect("test lock should be healthy");
            panic!("poison test");
        }));

        let mut next_log = match next_log.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        assert!(!should_log_request(
            &Method::GET,
            "/metrics",
            StatusCode::OK,
            Instant::now(),
            &mut next_log,
        ));
    }

    #[tokio::test]
    async fn metrics_route_returns_prometheus_exposition() {
        let recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            metrics::describe_gauge!("room_temperature", "Temperature in the room");
            metrics::gauge!("room_temperature", "name" => "test-room").set(21.5);
        });
        let response = build_router(AppState::new(recorder.handle(), Instant::now()))
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
