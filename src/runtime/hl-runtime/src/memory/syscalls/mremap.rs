//! mremap plan execution for the memory syscall surface.

use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{Errno, GuestMemory, LinuxResult, MemoryAbi};
use hl_memory::{Backing, MapRequest, MappingHost, Placement, Protection};

use crate::RuntimeMemorySyscalls;
use crate::memory::{AnonymousMemoryLease, errno::ErrorMap};

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    pub(super) fn mremap(&self, arguments: [u64; 6]) -> LinuxResult {
        let result = self.mremap_result(arguments);
        hl_log::hl_debug!(
            hl_log::tag::MEMORY,
            "mremap address={:#x} old_length={:#x} new_length={:#x} flags={:#x} destination={:#x} result={:#x}",
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
            result.encode(),
        );
        result
    }

    fn mremap_result(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = match MemoryAbi::<M>::mremap(
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3] as u32,
            arguments[4],
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let resolution = match self
            .coordinator
            .ledger()
            .resolve(plan.old_range.start(), Protection::NONE)
        {
            Some(value) if value.contiguous >= plan.old_range.length() => value,
            _ => return LinuxResult::Error(Errno::EFAULT),
        };
        // Linux DONTUNMAP is deliberately narrower than an ordinary move: the
        // source must be private anonymous memory. Reject other backing kinds
        // before staging a host operation so a failed request cannot replace a
        // destination or alter either mapping.
        if plan.keep_old && !matches!(resolution.region.backing(), Backing::Anonymous { shared: false, .. }) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if plan.new_length <= plan.old_range.length()
            && plan.requested_new_length <= plan.requested_old_length
            && plan.fixed.is_none()
            && !plan.keep_old
        {
            return self.shrink_remap(plan);
        }
        if let Some(destination) = plan.fixed {
            return self.relocate_remap(
                plan.old_range,
                plan.new_length,
                plan.requested_new_length,
                Placement::Fixed(destination),
                plan.keep_old,
                resolution,
            );
        }
        if plan.keep_old {
            return self.relocate_remap(
                plan.old_range,
                plan.new_length,
                plan.requested_new_length,
                Placement::Anywhere {
                    minimum: plan.old_range.end(),
                    maximum: GuestAddress::new(self.address_limit),
                    hint: None,
                },
                true,
                resolution,
            );
        }
        let extension = self.extend_remap(plan, resolution);
        if extension != LinuxResult::Error(Errno::EEXIST) || !plan.may_move {
            return extension;
        }
        self.relocate_remap(
            plan.old_range,
            plan.new_length,
            plan.requested_new_length,
            Placement::Anywhere {
                minimum: GuestAddress::new(4096),
                maximum: GuestAddress::new(self.address_limit),
                hint: None,
            },
            plan.keep_old,
            resolution,
        )
    }
    fn shrink_remap(&self, plan: hl_linux::MremapPlan) -> LinuxResult {
        let removed_charge = AddressRange::nonempty(
            GuestAddress::new(plan.old_range.start().get() + plan.requested_new_length),
            plan.old_range
                .end()
                .get()
                .saturating_sub(plan.old_range.start().get().saturating_add(plan.requested_new_length)),
        )
        .ok();
        let logical_tail = AddressRange::nonempty(
            GuestAddress::new(plan.old_range.start().get() + plan.requested_new_length),
            plan.new_length.saturating_sub(plan.requested_new_length),
        )
        .ok();
        let page_tail = AddressRange::nonempty(
            GuestAddress::new(plan.old_range.start().get() + plan.new_length),
            plan.old_range.length().saturating_sub(plan.new_length),
        )
        .ok();
        if logical_tail.is_none() && page_tail.is_none() {
            return LinuxResult::Value(plan.old_range.start().get());
        }
        let regions = self.coordinator.ledger().regions();
        let before = AnonymousMemoryLease::total(&regions).unwrap_or(u64::MAX);
        let removed = removed_charge.map_or(0, |range| Self::charged_overlap(&regions, range));
        let mut batch = hl_memory::MappingBatch::new();
        if let Some(range) = page_tail {
            batch.push(hl_memory::MappingOperation::Unmap(range));
        }
        if let Some(range) = logical_tail {
            batch.push(hl_memory::MappingOperation::Uncharge(range));
        }
        self.accounted(before.saturating_sub(removed), || {
            self.coordinator.apply(&batch).map(|_| plan.old_range.start())
        })
    }

    fn extend_remap(&self, plan: hl_linux::MremapPlan, source: hl_memory::Resolution) -> LinuxResult {
        let old = plan.old_range;
        let extra = plan.new_length - old.length();
        let request = MapRequest {
            placement: Placement::FixedNoReplace(old.end()),
            length: extra,
            alignment: 4096,
            protection: source.region.protection(),
            backing: source.region.backing(),
            backing_offset: source.backing_offset + old.length(),
        };
        let reserved = source.region.reserved();
        let mut batch = hl_memory::MappingBatch::new();
        if extra != 0 {
            if reserved {
                batch.push(hl_memory::MappingOperation::MapCharged(request, 0));
            } else {
                batch.push(hl_memory::MappingOperation::Map(request));
            }
        }
        if reserved && plan.requested_new_length > plan.requested_old_length {
            let start = GuestAddress::new(old.start().get() + plan.requested_old_length);
            let Ok(added) = AddressRange::nonempty(start, plan.requested_new_length - plan.requested_old_length) else {
                return LinuxResult::Error(Errno::ENOMEM);
            };
            batch.push(hl_memory::MappingOperation::Charge(added));
        }
        let before = AnonymousMemoryLease::total(&self.coordinator.ledger().regions()).unwrap_or(u64::MAX);
        let added = if reserved {
            plan.requested_new_length.saturating_sub(plan.requested_old_length)
        } else {
            0
        };
        self.accounted(before.saturating_add(added), || {
            self.coordinator.apply(&batch).map(|_| old.start())
        })
    }
}
