use std::time::{Duration, Instant};

use tapo::{responses::ChildDeviceHubResult::T31X, ApiClient, HubHandler};
use tokio::{sync::watch, time::sleep};

use crate::{config::Config, metrics, shutdown};

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_SUCCESS_LOG_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) async fn connect(config: Config) -> Result<HubHandler, tapo::Error> {
    tracing::info!("connecting to Tapo hub");
    ApiClient::new(config.tapo_username, config.tapo_password)
        .h100(config.tapo_hub_ip)
        .await
}

pub(crate) async fn run(hub: HubHandler, mut shutdown: watch::Receiver<bool>) {
    let mut last_success_log = Instant::now() - POLL_SUCCESS_LOG_INTERVAL;

    loop {
        let devices = match tokio::select! {
            result = hub.get_child_device_list() => result,
            _ = shutdown::changed(&mut shutdown) => {
                tracing::info!("Tapo polling task stopped");
                return;
            }
        } {
            Ok(devices) => devices,
            Err(error) => {
                tracing::error!(%error, "failed to poll Tapo hub");
                tokio::select! {
                    _ = sleep(POLL_INTERVAL) => {}
                    _ = shutdown::changed(&mut shutdown) => {
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
                metrics::update_t31x(&device);
                updated_devices += 1;
            }
        }

        if last_success_log.elapsed() >= POLL_SUCCESS_LOG_INTERVAL {
            tracing::info!(updated_devices, "updated Tapo metrics");
            last_success_log = Instant::now();
        }

        tokio::select! {
            _ = sleep(POLL_INTERVAL) => {}
            _ = shutdown::changed(&mut shutdown) => {
                tracing::info!("Tapo polling task stopped");
                return;
            }
        }
    }
}
