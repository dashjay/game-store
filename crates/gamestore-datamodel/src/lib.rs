//! `gamestore-datamodel` — Redis type / command layer.
//!
//! Skeleton crate introduced in **I-01**. The `CommandRegistry` and the
//! String/Hash/... command handlers that translate Redis commands into engine
//! operations land in **I-04** (Set/ZSet/List follow in **I-06**). Left empty
//! for now to keep the workspace buildable and boundaries aligned with the
//! plan (§2.1).
#![forbid(unsafe_code)]

/// Crate name, exposed for wiring/smoke assertions until the command API exists.
pub const CRATE_NAME: &str = "gamestore-datamodel";
