use std::time::Instant;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use metrics_exporter_prometheus::PrometheusHandle;

#[cfg(test)]
use metrics_exporter_prometheus::PrometheusBuilder;

#[derive(Clone)]
pub(crate) struct AppState {
    prometheus_handle: PrometheusHandle,
}

impl AppState {
    pub(crate) fn new(prometheus_handle: PrometheusHandle) -> Self {
        Self { prometheus_handle }
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

async fn request_logging(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status();
    let latency = started.elapsed();

    if status != StatusCode::OK {
        tracing::warn!(
            %method,
            %path,
            %status,
            latency_ms = latency.as_millis(),
            "HTTP request failed"
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        let recorder = PrometheusBuilder::new().build_recorder();
        AppState::new(recorder.handle())
    }

    #[tokio::test]
    async fn metrics_route_returns_prometheus_exposition() {
        let recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&recorder, || {
            metrics::describe_gauge!("room_temperature", "Temperature in the room");
            metrics::gauge!("room_temperature", "name" => "test-room").set(21.5);
        });
        let response = build_router(AppState::new(recorder.handle()))
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
