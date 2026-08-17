use std::{error::Error, fmt, net::IpAddr, str::FromStr};

use serde::Deserialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) tapo_username: String,
    pub(crate) tapo_password: String,
    pub(crate) tapo_hub_ip: String,
    pub(crate) rust_log: String,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(rename = "tapo_username")]
    tapo_username: Option<String>,
    #[serde(rename = "tapo_password")]
    tapo_password: Option<String>,
    #[serde(rename = "tapo_hub_ip")]
    tapo_hub_ip: Option<String>,
    #[serde(rename = "rust_log", default)]
    rust_log: Option<String>,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        let raw = envy::from_env::<RawConfig>().map_err(ConfigError::Environment)?;
        Self::try_from_raw(raw)
    }

    fn try_from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let tapo_username = required_value(raw.tapo_username, "TAPO_USERNAME")?;
        let tapo_password = required_value(raw.tapo_password, "TAPO_PASSWORD")?;
        let tapo_hub_ip = required_value(raw.tapo_hub_ip, "TAPO_HUB_IP")?;
        let tapo_hub_ip = IpAddr::from_str(&tapo_hub_ip)
            .map_err(|_| ConfigError::InvalidHubIp(tapo_hub_ip.clone()))?
            .to_string();
        let rust_log = raw.rust_log.unwrap_or_else(|| "info".to_owned());

        EnvFilter::try_new(&rust_log)
            .map_err(|error| ConfigError::InvalidLogFilter(error.to_string()))?;

        Ok(Self {
            tapo_username,
            tapo_password,
            tapo_hub_ip,
            rust_log,
        })
    }
}

fn required_value(value: Option<String>, name: &'static str) -> Result<String, ConfigError> {
    let value = value.ok_or(ConfigError::Missing(name))?;
    if value.trim().is_empty() {
        return Err(ConfigError::Empty(name));
    }
    Ok(value)
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Environment(envy::Error),
    Missing(&'static str),
    Empty(&'static str),
    InvalidHubIp(String),
    InvalidLogFilter(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment(error) => write!(formatter, "failed to parse environment: {error}"),
            Self::Missing(name) => write!(formatter, "{name} is required"),
            Self::Empty(name) => write!(formatter, "{name} must not be empty"),
            Self::InvalidHubIp(value) => {
                write!(formatter, "TAPO_HUB_IP is not a valid IP address: {value}")
            }
            Self::InvalidLogFilter(error) => write!(formatter, "RUST_LOG is invalid: {error}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Environment(error) => Some(error),
            Self::Missing(_)
            | Self::Empty(_)
            | Self::InvalidHubIp(_)
            | Self::InvalidLogFilter(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_config() -> RawConfig {
        RawConfig {
            tapo_username: Some("user@example.com".to_owned()),
            tapo_password: Some("secret".to_owned()),
            tapo_hub_ip: Some("192.168.1.100".to_owned()),
            rust_log: None,
        }
    }

    #[test]
    fn parses_required_values_and_defaults_log_filter() {
        let raw: RawConfig = envy::from_iter([
            ("TAPO_USERNAME".to_owned(), "user@example.com".to_owned()),
            ("TAPO_PASSWORD".to_owned(), "secret".to_owned()),
            ("TAPO_HUB_IP".to_owned(), "192.168.1.100".to_owned()),
        ])
        .expect("environment should deserialize");
        let config = Config::try_from_raw(raw).expect("config should be valid");

        assert_eq!(config.tapo_username, "user@example.com");
        assert_eq!(config.tapo_password, "secret");
        assert_eq!(config.tapo_hub_ip, "192.168.1.100");
        assert_eq!(config.rust_log, "info");
    }

    #[test]
    fn normalizes_valid_ipv6_addresses() {
        let mut raw = raw_config();
        raw.tapo_hub_ip = Some("2001:0db8:0:0:0:0:0:1".to_owned());

        let config = Config::try_from_raw(raw).expect("config should be valid");

        assert_eq!(config.tapo_hub_ip, "2001:db8::1");
    }

    #[test]
    fn rejects_missing_required_values() {
        let mut raw = raw_config();
        raw.tapo_username = None;
        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Missing("TAPO_USERNAME"))
        ));

        let mut raw = raw_config();
        raw.tapo_password = None;
        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Missing("TAPO_PASSWORD"))
        ));

        let mut raw = raw_config();
        raw.tapo_hub_ip = None;
        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Missing("TAPO_HUB_IP"))
        ));
    }

    #[test]
    fn rejects_empty_required_values() {
        let mut raw = raw_config();
        raw.tapo_password = Some("  \t".to_owned());

        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Empty("TAPO_PASSWORD"))
        ));

        let mut raw = raw_config();
        raw.tapo_username = Some("  \t".to_owned());
        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Empty("TAPO_USERNAME"))
        ));

        let mut raw = raw_config();
        raw.tapo_hub_ip = Some("  \t".to_owned());
        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::Empty("TAPO_HUB_IP"))
        ));
    }

    #[test]
    fn rejects_invalid_hub_ip() {
        let mut raw = raw_config();
        raw.tapo_hub_ip = Some("hub.local".to_owned());

        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::InvalidHubIp(value)) if value == "hub.local"
        ));
    }

    #[test]
    fn rejects_invalid_log_filter() {
        let mut raw = raw_config();
        raw.rust_log = Some("=".to_owned());

        assert!(matches!(
            Config::try_from_raw(raw),
            Err(ConfigError::InvalidLogFilter(_))
        ));
    }

    #[test]
    fn configuration_errors_do_not_include_passwords() {
        let mut raw = raw_config();
        raw.tapo_hub_ip = Some("not-an-ip".to_owned());
        let error = Config::try_from_raw(raw).expect_err("config should be invalid");

        assert!(!error.to_string().contains("secret"));
    }
}
