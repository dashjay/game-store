//! `gamestore-protocol` — RESP2/RESP3 wire protocol for GameStore.
//!
//! Skeleton crate introduced in **I-01**. The full sans-IO encoder/decoder
//! (ported and hardened from `spike/rust/src/resp.rs`) plus the tokio `Framed`
//! adapters land in **I-02**. Until then this crate is intentionally empty so
//! the workspace stays buildable and crate boundaries match the plan (§2.1).
#![forbid(unsafe_code)]

/// Crate name, exposed so downstream code and tests can assert wiring works
/// before the real protocol types exist.
pub const CRATE_NAME: &str = "gamestore-protocol";
