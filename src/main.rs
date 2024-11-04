use std::{env, time::Duration};

use metrics_exporter_prometheus::PrometheusBuilder;
use tapo::responses::ChildDeviceHubResult::T310;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tapo_username = env::var("TAPO_USERNAME").expect("TAPO_USERNAME is required");
    let tapo_password = env::var("TAPO_PASSWORD").expect("TAPO_PASSWORD is required");
    let tapo_hub_ip = env::var("TAPO_HUB_IP").expect("TAPO_HUB_IP is required");

    PrometheusBuilder::new()
        .idle_timeout(
            metrics_util::MetricKindMask::COUNTER | metrics_util::MetricKindMask::HISTOGRAM,
            Some(Duration::from_secs(10)),
        )
        .install()
        .expect("failed to install Prometheus recorder");

    metrics::describe_gauge!("room_temperature", "Temperature in the room");
    metrics::describe_gauge!("room_humidity", "Humidity in the room");

    let hub = tapo::ApiClient::new(tapo_username, tapo_password)
        .h100(tapo_hub_ip)
        .await?;

    loop {
        let devices = hub.get_child_device_list().await?;
        for device in devices {
            if let T310(device) = device {
                metrics::gauge!("room_temperature", "name" => device.nickname.clone())
                    .set(device.current_temperature as f64);

                metrics::gauge!("room_humidity", "name" => device.nickname.clone())
                    .set(device.current_humidity as f64);
            }
        }

        sleep(Duration::from_secs(5)).await;
    }
}
