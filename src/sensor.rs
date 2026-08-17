use std::time::Duration;

use tapo::{responses::ChildDeviceHubResult::T31X, ApiClient, HubHandler};
use tokio::{sync::watch, time::sleep};

use crate::{config::Config, metrics, shutdown};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) async fn connect(config: Config) -> Result<HubHandler, tapo::Error> {
    tracing::info!("connecting to Tapo hub");
    ApiClient::new(config.tapo_username, config.tapo_password)
        .h100(config.tapo_hub_ip)
        .await
}

pub(crate) async fn run(hub: HubHandler, mut shutdown: watch::Receiver<bool>) {
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

        for device in devices {
            if let T31X(device) = device {
                metrics::update_t31x(&device);
            }
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
