use std::{error::Error, time::Duration};

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tapo::responses::T31XResult;
use tokio::{sync::watch, task::JoinHandle};

const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn install_recorder() -> Result<PrometheusHandle, Box<dyn Error>> {
    let handle = PrometheusBuilder::new()
        .idle_timeout(
            metrics_util::MetricKindMask::COUNTER | metrics_util::MetricKindMask::HISTOGRAM,
            Some(Duration::from_secs(10)),
        )
        .install_recorder()?;

    Ok(handle)
}

pub(crate) fn describe_metrics() {
    metrics::describe_gauge!("room_temperature", "Temperature in the room");
    metrics::describe_gauge!("room_humidity", "Humidity in the room");
}

pub(crate) fn update_t31x(device: &T31XResult) {
    metrics::gauge!("room_temperature", "name" => device.nickname.clone())
        .set(device.current_temperature as f64);
    metrics::gauge!("room_humidity", "name" => device.nickname.clone())
        .set(device.current_humidity as f64);
}

pub(crate) fn spawn_upkeep(
    handle: PrometheusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(UPKEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => handle.run_upkeep(),
                _ = crate::shutdown::changed(&mut shutdown) => {
                    tracing::info!("Prometheus upkeep task stopped");
                    break;
                }
            }
        }
    })
}
