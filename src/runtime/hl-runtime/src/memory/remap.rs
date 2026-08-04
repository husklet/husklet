use hl_isa::AddressRange;
use hl_linux::{GuestMemory, LinuxResult};
use hl_memory::{MapRequest, MappingHost, Placement};

use crate::{RuntimeMemorySyscalls, memory::errno::ErrorMap};

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    pub(super) fn relocate_remap(
        &self,
        old: AddressRange,
        new_length: u64,
        placement: Placement,
        keep_old: bool,
        source: hl_memory::Resolution,
    ) -> LinuxResult {
        let request = MapRequest {
            placement,
            length: new_length,
            alignment: 4096,
            protection: source.region.protection(),
            backing: source.region.backing(),
            backing_offset: source.backing_offset,
        };
        match self.coordinator.remap(old, request, keep_old) {
            Ok(address) => LinuxResult::Value(address.get()),
            Err(error) => LinuxResult::Error(ErrorMap::ledger(error)),
        }
    }
}
