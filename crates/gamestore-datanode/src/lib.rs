//! `gamestore-datanode` — the DataNode service.
//!
//! Started in **I-01** as a minimal tokio RESP server; **I-02** moved wire
//! handling to the hardened [`gamestore_protocol`] codec (RESP2/RESP3 with
//! per-connection `HELLO` negotiation). As of **I-05** this is the single-node
//! MVP assembly: it opens one shared [`gamestore_engine::Store`] (RocksDB) and
//! dispatches every data command through the
//! [`gamestore_datamodel::CommandRegistry`], keeping only connection-scoped
//! commands (`HELLO`/`QUIT` + `CLIENT`/`SELECT`/`COMMAND` housekeeping) and
//! database admin (`FLUSHDB`/`FLUSHALL`) in this layer. Later MRs turn it into
//! the multi-replica, WAL-backed DataNode described in
//! `docs/design/02-architecture.md` §3.2 (see the `Core` note in [`server`]).
#![forbid(unsafe_code)]

pub mod server;

pub use server::serve;
