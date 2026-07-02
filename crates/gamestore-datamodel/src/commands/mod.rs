//! The I-04 command set, grouped by family. Each module registers its commands
//! into a [`CommandRegistry`]; [`register_all`] wires the full set.

use gamestore_engine::GeneralEngine;

use crate::registry::CommandRegistry;

pub mod connectivity;

/// Register every I-04 command family (connectivity, String + TTL, Hash,
/// introspection) into `reg`.
pub fn register_all<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    connectivity::register(reg);
}
