//! `gamestore-datanode` — the DataNode service.
//!
//! Started in **I-01** as a minimal tokio RESP server. As of **I-02** it drives
//! connections through the hardened [`gamestore_protocol`] codec (RESP2/RESP3),
//! answers the handshake/liveness subset (`PING`/`ECHO`/`HELLO`/`QUIT`) and
//! negotiates the protocol version per connection via `HELLO`. Later MRs turn it
//! into the single-node MVP (command registry + engine in I-04/I-05) and
//! eventually the multi-replica, WAL-backed DataNode described in
//! `docs/design/02-architecture.md` §3.2.
#![forbid(unsafe_code)]

pub mod server;

pub use server::serve;
