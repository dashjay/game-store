//! `gamestore-datanode` — the DataNode service.
//!
//! In **I-01** this is a minimal tokio RESP server that answers `PING` with
//! `PONG`, proving the accept-loop → protocol → reply wiring. Later MRs turn it
//! into the single-node MVP (command registry + engine in I-05) and eventually
//! the multi-replica, WAL-backed DataNode described in
//! `docs/design/02-architecture.md` §3.2.
#![forbid(unsafe_code)]

pub mod resp;
pub mod server;

pub use server::serve;
