use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use hl_isa::GuestAddress;
use hl_memory::{FutexIdentity, MappingCoordinator, MemoryAccessHost};
use hl_sync::FutexKey;

pub(super) struct Binding<H: MemoryAccessHost> {
    pub(super) memory: Weak<MappingCoordinator<H>>,
    pub(super) address: GuestAddress,
    pub(super) private: bool,
    pub(super) identity: FutexIdentity,
}
impl<H: MemoryAccessHost> Binding<H> {
    pub(super) fn matches(&self, memory: &Arc<MappingCoordinator<H>>, address: GuestAddress) -> bool {
        if self.address != address {
            return false;
        }
        let Some(candidate) = self.memory.upgrade() else {
            return false;
        };
        Arc::ptr_eq(&candidate, memory)
    }
}
pub(super) struct KeyState<H: MemoryAccessHost> {
    pub(super) next_shared: u64,
    pub(super) shared: BTreeMap<FutexIdentity, u64>,
    pub(super) bindings: BTreeMap<FutexKey, Vec<Binding<H>>>,
}

impl<H: MemoryAccessHost> Default for KeyState<H> {
    fn default() -> Self {
        Self {
            next_shared: 0,
            shared: BTreeMap::new(),
            bindings: BTreeMap::new(),
        }
    }
}
