//! Metrics façade built on the `metrics` facade crate + Prometheus exporter.
//!
//! Aligns with `docs/design/08-observability-ops.md`. I-01 installs the global
//! Prometheus recorder and hands back a [`PrometheusHandle`] that can render the
//! exposition text. Wiring a `/metrics` HTTP endpoint and the concrete metric
//! set (QPS, latency histograms, engine stats, …) is I-07.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::error::{Error, Result};

/// Install the global Prometheus recorder and return its render handle.
///
/// The handle's [`PrometheusHandle::render`] produces the Prometheus text
/// exposition format. This does **not** start an HTTP server (that is I-07);
/// it only makes `metrics::counter!`/`gauge!`/`histogram!` calls record into a
/// Prometheus-compatible registry.
pub fn init() -> Result<PrometheusHandle> {
    PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| Error::Observability(format!("installing prometheus recorder: {e}")))
}
