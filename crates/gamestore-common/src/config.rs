//! Configuration façade: `serde` + TOML with environment-variable overrides.
//!
//! Aligns with cloud-native deployment (ConfigMap / env). This is the I-01
//! skeleton — just enough structure for the DataNode to bind a port and set up
//! logging/metrics. Later MRs extend these structs (engine tuning, replica
//! topology, quorum, …) without changing the loading mechanism.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Environment-variable prefix for overrides, e.g. `GAMESTORE_SERVER__PORT`.
const ENV_PREFIX: &str = "GAMESTORE_";

/// Top-level GameStore configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Network listener settings.
    pub server: ServerConfig,
    /// Structured logging settings.
    pub logging: LoggingConfig,
    /// Metrics exporter settings.
    pub metrics: MetricsConfig,
}

/// RESP server listener configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Bind address for the RESP listener.
    pub bind: String,
    /// TCP port for the RESP listener (Redis default is 6379).
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        // 6380 by default to avoid clashing with a local Redis on 6379.
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 6380,
        }
    }
}

impl ServerConfig {
    /// `bind:port` string suitable for `TcpListener::bind`.
    pub fn addr(&self) -> String {
        format!("{}:{}", self.bind, self.port)
    }
}

/// Structured logging configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Default log level / `EnvFilter` directive (e.g. `info`, `gamestore=debug`).
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            level: "info".to_string(),
        }
    }
}

/// Metrics exporter configuration (Prometheus).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    /// Whether to install the Prometheus recorder at startup.
    pub enabled: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        MetricsConfig { enabled: true }
    }
}

impl Config {
    /// Load configuration.
    ///
    /// When `path` is `Some`, the TOML file is read and parsed; otherwise
    /// defaults are used. In both cases environment variables prefixed with
    /// `GAMESTORE_` are applied as overrides on top.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let mut cfg = match path {
            Some(p) => Self::from_file(p)?,
            None => Config::default(),
        };
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    /// Parse a TOML config file.
    pub fn from_file(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("reading {}: {e}", path.display())))?;
        Self::from_toml(&text)
    }

    /// Parse configuration from a TOML string.
    pub fn from_toml(text: &str) -> Result<Config> {
        toml::from_str(text).map_err(|e| Error::config(format!("parsing TOML: {e}")))
    }

    /// Apply `GAMESTORE_*` environment-variable overrides.
    ///
    /// Recognised keys (nested via `__`):
    /// - `GAMESTORE_SERVER__BIND`, `GAMESTORE_SERVER__PORT`
    /// - `GAMESTORE_LOGGING__LEVEL`
    /// - `GAMESTORE_METRICS__ENABLED`
    fn apply_env_overrides(&mut self) {
        if let Some(v) = env_var("SERVER__BIND") {
            self.server.bind = v;
        }
        if let Some(v) = env_var("SERVER__PORT") {
            if let Ok(port) = v.parse() {
                self.server.port = port;
            }
        }
        if let Some(v) = env_var("LOGGING__LEVEL") {
            self.logging.level = v;
        }
        if let Some(v) = env_var("METRICS__ENABLED") {
            if let Ok(enabled) = v.parse() {
                self.metrics.enabled = enabled;
            }
        }
    }
}

/// Read `GAMESTORE_<suffix>`, returning `None` when unset or empty.
fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("{ENV_PREFIX}{suffix}"))
        .ok()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.server.bind, "127.0.0.1");
        assert_eq!(cfg.server.port, 6380);
        assert_eq!(cfg.server.addr(), "127.0.0.1:6380");
        assert_eq!(cfg.logging.level, "info");
        assert!(cfg.metrics.enabled);
    }

    #[test]
    fn parses_partial_toml_with_defaults() {
        let cfg = Config::from_toml("[server]\nport = 7000\n").unwrap();
        assert_eq!(cfg.server.port, 7000);
        // Untouched fields keep their defaults.
        assert_eq!(cfg.server.bind, "127.0.0.1");
        assert!(cfg.metrics.enabled);
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = Config::from_toml("[server]\nnope = 1\n").unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }
}
