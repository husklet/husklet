use std::sync::atomic::Ordering;

use crate::{DescriptorSnapshot, DescriptorTable};

impl DescriptorTable {
    /// Captures every active descriptor without changing table or OFD state.
    #[must_use]
    pub fn active_snapshots(&self) -> Vec<DescriptorSnapshot> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .iter()
            .map(|(number, descriptor)| {
                let description_state = descriptor
                    .description
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                DescriptorSnapshot {
                    number: *number,
                    description_identity: descriptor.description.identity,
                    offset: description_state.offset,
                    status: description_state.status,
                    flags: descriptor.flags,
                    descriptor_generation: descriptor.generation,
                    description_generation: descriptor.description.generation,
                    descriptor_references: descriptor.description.descriptor_references.load(Ordering::Acquire),
                    kind: descriptor.description.object.kind(),
                    flock_token: descriptor.description.identity,
                }
            })
            .collect()
    }
}
