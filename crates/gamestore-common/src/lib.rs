//! `gamestore-common` — shared infrastructure for the GameStore workspace.
//!
//! Introduced in **I-01** as the base every other crate builds on. It provides
//! the cross-cutting façades defined in the implementation plan (§1):
//!
//! - [`error`] — the unified [`Error`] type (`thiserror`) and [`Result`] alias.
//! - [`config`] — `serde` + TOML configuration with env-var overrides.
//! - [`telemetry`] — structured logging via `tracing`.
//! - [`metrics`] — Prometheus metrics exporter façade.
//!
//! These are deliberately thin in I-01; later MRs extend them in place without
//! changing the boundaries.
#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod metrics;
pub mod telemetry;

pub use config::Config;
pub use error::{Error, Result};
