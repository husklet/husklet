use hl_isa::AddressRange;
use hl_linux::{GuestMemory, LinuxResult};
use hl_memory::{MapRequest, MappingHost, Placement};

use crate::RuntimeMemorySyscalls;

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    pub(super) fn relocate_remap(
        &self,
        old: AddressRange,
        new_length: u64,
        requested_new_length: u64,
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
        let regions = self.coordinator.ledger().regions();
        let before = super::AnonymousMemoryLease::total(&regions).unwrap_or(u64::MAX);
        let source_charge = Self::charged_overlap(&regions, old);
        let destination_charge = match placement {
            Placement::Fixed(start) => AddressRange::nonempty(start, new_length)
                .ok()
                .map_or(0, |range| Self::charged_overlap(&regions, range)),
            Placement::FixedNoReplace(_) | Placement::Anywhere { .. } => 0,
        };
        let reserved = source.region.reserved();
        let new_charge = if reserved { requested_new_length } else { 0 };
        let removed = destination_charge.saturating_add(if keep_old { 0 } else { source_charge });
        let target = before.saturating_sub(removed).saturating_add(new_charge);
        self.accounted(target, || {
            if reserved {
                self.coordinator.remap_charged(old, request, keep_old, new_charge)
            } else {
                self.coordinator.remap(old, request, keep_old)
            }
        })
    }
}
