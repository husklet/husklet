//! `mmap` and `memfd_create` backing selection for the memory syscall surface.

use std::sync::atomic::Ordering;

use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{Errno, GuestMemory, LinuxResult, MapSource, MemoryAbi};
use hl_memory::{Backing, MapRequest, MappingHost, Placement, SharedError};

use crate::RuntimeMemorySyscalls;
use crate::memory::{AnonymousMemoryLease, charge::ChargeTransitionError, errno::ErrorMap};

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    pub(super) fn mmap(&self, arguments: [u64; 6], pages: bool) -> LinuxResult {
        let result = self.mmap_result(arguments, pages);
        hl_log::hl_debug!(
            hl_log::tag::MEMORY,
            "mmap address={:#x} length={:#x} protection={:#x} flags={:#x} descriptor={} offset={:#x} pages={pages} result={:#x}",
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4] as i32,
            arguments[5],
            result.encode(),
        );
        result
    }

    fn mmap_result(&self, arguments: [u64; 6], pages: bool) -> LinuxResult {
        // Linux acquires a file reference before validating the mapping
        // length. Preserve that observable ordering: a stale descriptor on a
        // file-backed zero-length request is EBADF, not EINVAL. Anonymous
        // mappings deliberately ignore the descriptor argument.
        if arguments[3] as u32 & 0x20 == 0 && self.descriptors.pin(arguments[4] as i32).is_err() {
            return LinuxResult::Error(Errno::EBADF);
        }
        let abi = MemoryAbi::new(&self.memory, self.architecture);
        let plan = if pages {
            abi.mmap2(
                arguments[0],
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
                arguments[4] as i32,
                arguments[5],
            )
        } else {
            abi.mmap(
                arguments[0],
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
                arguments[4] as i32,
                arguments[5],
            )
        };
        let plan = match plan {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        if plan.length > self.address_limit {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        let placement = match plan.placement {
            Placement::Fixed(address) | Placement::FixedNoReplace(address) if address.get() < self.minimum_address => {
                return LinuxResult::Error(Errno::EPERM);
            }
            Placement::Fixed(address) | Placement::FixedNoReplace(address)
                if address
                    .get()
                    .checked_add(plan.length)
                    .is_none_or(|end| end > self.address_limit) =>
            {
                return LinuxResult::Error(Errno::ENOMEM);
            }
            Placement::Anywhere { minimum, hint, .. } => Placement::Anywhere {
                minimum: minimum.max(GuestAddress::new(self.minimum_address)),
                maximum: GuestAddress::new(self.address_limit),
                hint: hint.filter(|address| address.get() >= self.minimum_address),
            },
            placement => placement,
        };
        let backing = match self.backing(plan) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let request = MapRequest {
            placement,
            length: plan.length,
            alignment: 4096,
            protection: plan.protection,
            backing,
            backing_offset: plan.offset,
        };
        let charge = matches!(plan.source, MapSource::Anonymous { .. }) && !plan.no_reserve;
        let replaced = match placement {
            Placement::Fixed(start) => AddressRange::nonempty(start, plan.length).ok(),
            Placement::FixedNoReplace(_) | Placement::Anywhere { .. } => None,
        };
        let before = AnonymousMemoryLease::total(&self.coordinator.ledger().regions()).unwrap_or(u64::MAX);
        let removed = replaced.map_or(0, |range| {
            Self::charged_overlap(&self.coordinator.ledger().regions(), range)
        });
        let new_charge = if charge { plan.requested_length } else { 0 };
        let target = before.saturating_sub(removed).saturating_add(new_charge);
        let operation = || {
            if charge {
                self.coordinator.map_charged(request, new_charge)
            } else {
                self.coordinator.map(request)
            }
        };
        let result = match &self.anonymous_charge {
            Some(lease) => lease.transition(target, operation),
            None => operation().map_err(ChargeTransitionError::Operation),
        };
        match result {
            Ok(address) => LinuxResult::Value(address.get()),
            Err(ChargeTransitionError::Limit) => LinuxResult::Error(Errno::ENOMEM),
            Err(ChargeTransitionError::Operation(error)) => LinuxResult::Error(ErrorMap::ledger(error)),
        }
    }

    fn backing(&self, plan: hl_linux::MmapPlan) -> Result<Backing, Errno> {
        match plan.source {
            MapSource::Anonymous { shared } => self.anonymous_backing(shared),
            MapSource::File { descriptor, shared } => self.file_backing(plan, descriptor, shared),
        }
    }

    fn anonymous_backing(&self, shared: bool) -> Result<Backing, Errno> {
        let local = self.next_anonymous.fetch_add(1, Ordering::Relaxed);
        let identity = if shared {
            let owner = self.shared.as_ref().map(|(_, owner)| *owner).ok_or(Errno::ENOSYS)?;
            owner.rotate_left(32) ^ local
        } else {
            local
        };
        Ok(Backing::Anonymous { identity, shared })
    }

    fn file_backing(&self, plan: hl_linux::MmapPlan, descriptor: i32, shared: bool) -> Result<Backing, Errno> {
        let lease = self.descriptors.pin(descriptor).map_err(|_| Errno::EBADF)?;
        let access = lease.status().bits() & StatusFlags::ACCESS_MODE_MASK;
        let readable = access == 0 || access == 2;
        let writable = access == 2;
        if !readable || (shared && plan.protection.contains(hl_memory::Protection::WRITE) && !writable) {
            return Err(Errno::EACCES);
        }
        match self
            .memfds
            .backing(lease.description_identity(), plan.offset, plan.length, shared)
        {
            Ok(value) => return Ok(value),
            Err(SharedError::Range) => return Err(Errno::ENOSYS),
            Err(SharedError::NotFound) => {}
            Err(error) => return Err(ErrorMap::ledger(hl_memory::MemoryError::Shared(error))),
        }
        let source = self.descriptor_source.as_ref().ok_or(Errno::ENOSYS)?;
        source
            .backing(
                &lease,
                plan.offset,
                plan.length,
                shared,
                plan.protection.contains(hl_memory::Protection::WRITE),
            )
            .map_err(ErrorMap::runtime)
    }

    pub(super) fn memfd_create(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = match MemoryAbi::new(&self.memory, self.architecture).memfd_create(arguments[0], arguments[1] as u32)
        {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        if plan.huge_page.is_some() {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        if self.shared.is_none() {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let object = match self.memfds.create(plan.allow_sealing) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::ledger(hl_memory::MemoryError::Shared(error))),
        };
        let local = DescriptorFlags::from_bits(if plan.close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let status = StatusFlags::from_bits(2);
        let install = match self.descriptors.prepare_open(0, object.clone(), status, local) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FilesystemErrno::descriptor(error)),
        };
        let identity = install.description_identity();
        if self.memfds.register(identity, object).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        LinuxResult::Value(install.publish() as u64)
    }
}
