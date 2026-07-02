//! The I-04 command set, grouped by family. Each module registers its commands
//! into a [`CommandRegistry`]; [`register_all`] wires the full set.

use gamestore_engine::GeneralEngine;

use crate::registry::CommandRegistry;

pub mod admin;
pub mod connectivity;
pub mod hash;
pub mod list;
pub mod set;
pub mod string;
pub mod zset;

/// Register every command family (connectivity, String + TTL, Hash,
/// Set/ZSet/List, introspection) into `reg`.
pub fn register_all<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    connectivity::register(reg);
    string::register(reg);
    hash::register(reg);
    set::register(reg);
    zset::register(reg);
    list::register(reg);
    admin::register(reg);
}
