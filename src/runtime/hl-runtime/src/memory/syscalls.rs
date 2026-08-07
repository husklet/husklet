use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_descriptor::{DescriptorFlags, DescriptorTable, StatusFlags};
use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{
    Errno, GuestArchitecture, GuestIovec, GuestMarshaller, GuestMemory, IOV_MAXIMUM, LinuxResult, MapSource, MemoryAbi,
    MemorySyscalls, SyscallOperation,
};
use hl_memory::{
    Backing, MapRequest, MappingCoordinator, MappingHost, Placement, Protection, SharedError, SharedObjectStore,
};

use crate::{BrkRegion, DescriptorMappingSource, MemfdRegistry, RuntimeMemoryHost};

use super::{AnonymousMemoryLease, charge::ChargeTransitionError, errno::ErrorMap};

const MMAP_MINIMUM_ADDRESS: u64 = 32 * 1024;

pub struct RuntimeMemorySyscalls<H: MappingHost, M: GuestMemory> {
    pub(crate) coordinator: Arc<MappingCoordinator<H>>,
    descriptors: Arc<DescriptorTable>,
    memory: M,
    architecture: GuestArchitecture,
    host: Option<Arc<dyn RuntimeMemoryHost>>,
    descriptor_source: Option<Arc<dyn DescriptorMappingSource>>,
    memfds: Arc<MemfdRegistry>,
    shared: Option<(Arc<SharedObjectStore>, u64)>,
    brk: Option<BrkRegion<H>>,
    anonymous_charge: Option<Arc<AnonymousMemoryLease>>,
    next_anonymous: AtomicU64,
    minimum_address: u64,
    address_limit: u64,
    process: Option<u32>,
}

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    fn get_mempolicy(&self, mode: u64) -> LinuxResult {
        if mode == 0 {
            return LinuxResult::Value(0);
        }
        let bytes = 0_i32.to_le_bytes();
        match hl_linux::GuestMarshaller::new(&self.memory, self.architecture).copy_struct_to(mode, &bytes) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    fn move_pages(&self, arguments: [u64; 6]) -> LinuxResult {
        let Ok(count) = usize::try_from(arguments[1]) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if count == 0 {
            return LinuxResult::Value(0);
        }
        let Some(pointer_bytes) = count.checked_mul(std::mem::size_of::<u64>()) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Some(status_bytes) = count.checked_mul(std::mem::size_of::<i32>()) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let mut pages = Vec::new();
        if pages.try_reserve_exact(pointer_bytes).is_err() {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        pages.resize(pointer_bytes, 0);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let copied = marshaller.copy_from(arguments[2], &mut pages);
        if copied.copied != pointer_bytes || copied.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if arguments[3] != 0 {
            let mut nodes = Vec::new();
            if nodes.try_reserve_exact(status_bytes).is_err() {
                return LinuxResult::Error(Errno::ENOMEM);
            }
            nodes.resize(status_bytes, 0);
            let copied = marshaller.copy_from(arguments[3], &mut nodes);
            if copied.copied != status_bytes || copied.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        if arguments[4] == 0 {
            return LinuxResult::Value(0);
        }
        let mut status = Vec::new();
        if status.try_reserve_exact(status_bytes).is_err() {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        for address in pages.chunks_exact(8).map(|bytes| {
            let bytes: [u8; 8] = bytes.try_into().expect("fixed pointer width");
            u64::from_le_bytes(bytes)
        }) {
            let residency = if self
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(address), Protection::READ)
                .is_some()
            {
                0_i32
            } else {
                -Errno::ENOENT.raw()
            };
            status.extend_from_slice(&residency.to_le_bytes());
        }
        let copied = marshaller.copy_to(arguments[4], &status);
        if copied.copied == status_bytes && copied.fault.is_none() {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EFAULT)
        }
    }

    pub fn new(
        coordinator: Arc<MappingCoordinator<H>>,
        descriptors: Arc<DescriptorTable>,
        memory: M,
        architecture: GuestArchitecture,
    ) -> Self {
        Self {
            coordinator,
            descriptors,
            memory,
            architecture,
            host: None,
            descriptor_source: None,
            memfds: Arc::new(MemfdRegistry::new()),
            shared: None,
            brk: None,
            anonymous_charge: None,
            next_anonymous: AtomicU64::new(1),
            minimum_address: MMAP_MINIMUM_ADDRESS,
            address_limit: u64::MAX & !4095,
            process: None,
        }
    }

    #[must_use]
    pub fn with_memfd_objects(
        mut self,
        store: Arc<SharedObjectStore>,
        owner: u64,
        registry: Arc<MemfdRegistry>,
    ) -> Self {
        let _ = registry.configure(Arc::clone(&store), owner);
        self.shared = Some((store, owner));
        self.memfds = registry;
        self
    }

    #[must_use]
    pub fn with_host(mut self, host: Arc<dyn RuntimeMemoryHost>) -> Self {
        self.host = Some(host);
        self
    }

    #[must_use]
    pub fn with_descriptor_source(mut self, source: Arc<dyn DescriptorMappingSource>) -> Self {
        self.descriptor_source = Some(source);
        self
    }

    #[must_use]
    pub fn with_brk(mut self, brk: BrkRegion<H>) -> Self {
        self.anonymous_charge = brk.lease();
        self.brk = Some(brk);
        self
    }

    #[must_use]
    pub fn anonymous_account(&self) -> Option<Arc<dyn crate::AnonymousMemoryAccount>> {
        self.anonymous_charge.as_ref().map(|lease| lease.account())
    }

    #[must_use]
    pub fn with_address_limit(mut self, limit: u64) -> Self {
        self.address_limit = limit & !4095;
        self
    }

    #[must_use]
    pub fn with_address_minimum(mut self, minimum: u64) -> Self {
        self.minimum_address = minimum.max(4096).saturating_add(4095) & !4095;
        self
    }

    #[must_use]
    pub fn with_process(mut self, process: u32) -> Self {
        self.process = Some(process);
        self
    }

    pub fn fork_clone(
        &self,
        coordinator: Arc<MappingCoordinator<H>>,
        descriptors: Arc<DescriptorTable>,
        memory: M,
        owner: u64,
    ) -> Result<Self, hl_memory::MemoryError> {
        let brk = self
            .brk
            .as_ref()
            .map(|value| value.fork(Arc::clone(&coordinator)))
            .transpose()?;
        let anonymous_charge = brk.as_ref().and_then(BrkRegion::lease);
        Ok(Self {
            coordinator,
            descriptors,
            memory,
            architecture: self.architecture,
            host: self.host.clone(),
            descriptor_source: self.descriptor_source.clone(),
            memfds: Arc::clone(&self.memfds),
            shared: self.shared.as_ref().map(|(store, _)| (Arc::clone(store), owner)),
            brk,
            anonymous_charge,
            next_anonymous: AtomicU64::new(self.next_anonymous.load(std::sync::atomic::Ordering::Relaxed)),
            minimum_address: self.minimum_address,
            address_limit: self.address_limit,
            process: self.process,
        })
    }

    fn process_vector(&self, arguments: [u64; 6], reading: bool) -> LinuxResult {
        if arguments[5] != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let (Ok(local_count), Ok(remote_count)) = (usize::try_from(arguments[2]), usize::try_from(arguments[4])) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if local_count > IOV_MAXIMUM || remote_count > IOV_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let local = match marshaller.iovecs(arguments[1], local_count) {
            Ok(plan) => plan.vectors,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let remote = match marshaller.iovecs(arguments[3], remote_count) {
            Ok(plan) => plan.vectors,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let target = arguments[0] as i32;
        if target <= 0 || self.process.is_none_or(|process| target as u32 != process) {
            return LinuxResult::Error(Errno::ESRCH);
        }
        if reading {
            self.copy_vectors(&remote, &local)
        } else {
            self.copy_vectors(&local, &remote)
        }
    }

    fn copy_vectors(&self, source: &[GuestIovec], destination: &[GuestIovec]) -> LinuxResult {
        let mut source_index = 0;
        let mut destination_index = 0;
        let mut source_offset = 0_u64;
        let mut destination_offset = 0_u64;
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        while source_index < source.len() && destination_index < destination.len() {
            let source_left = source[source_index].length - source_offset;
            let destination_left = destination[destination_index].length - destination_offset;
            let amount = source_left.min(destination_left).min(buffer.len() as u64) as usize;
            if amount == 0 {
                if source_left == 0 {
                    source_index += 1;
                    source_offset = 0;
                }
                if destination_left == 0 {
                    destination_index += 1;
                    destination_offset = 0;
                }
                continue;
            }
            let Some(source_address) = source[source_index].base.checked_add(source_offset) else {
                return Self::copy_result(total);
            };
            let read = match self.memory.read(source_address, &mut buffer[..amount]) {
                Ok(value) if value != 0 => value,
                _ => return Self::copy_result(total),
            };
            let Some(destination_address) = destination[destination_index].base.checked_add(destination_offset) else {
                return Self::copy_result(total);
            };
            let written = match self.memory.write(destination_address, &buffer[..read]) {
                Ok(value) if value != 0 => value,
                _ => return Self::copy_result(total),
            };
            total += written as u64;
            source_offset += written as u64;
            destination_offset += written as u64;
            if written != amount {
                return LinuxResult::Value(total);
            }
        }
        LinuxResult::Value(total)
    }

    fn copy_result(total: u64) -> LinuxResult {
        if total == 0 {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(total)
        }
    }

    #[must_use]
    pub fn memfd_registry(&self) -> Arc<MemfdRegistry> {
        Arc::clone(&self.memfds)
    }

    fn mmap(&self, arguments: [u64; 6], pages: bool) -> LinuxResult {
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
        let new_charge = charge.then_some(plan.requested_length).unwrap_or(0);
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

    fn memfd_create(&self, arguments: [u64; 6]) -> LinuxResult {
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

    fn range_operation(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let result = self.range_operation_result(name, arguments);
        hl_log::hl_debug!(
            hl_log::tag::MEMORY,
            "{name} address={:#x} length={:#x} protection={:#x} result={:#x}",
            arguments[0],
            arguments[1],
            arguments[2],
            result.encode(),
        );
        result
    }

    fn range_operation_result(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let plan = match name {
            "munmap" => MemoryAbi::<M>::munmap(arguments[0], arguments[1]),
            "mprotect" => match MemoryAbi::<M>::mprotect(arguments[0], arguments[1], arguments[2] as u32) {
                Ok(None) => return LinuxResult::Value(0),
                Ok(Some(value)) => Ok(value),
                Err(error) => Err(error),
            },
            _ => unreachable!(),
        };
        let plan = match plan {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let result = match plan.protection {
            Some(protection) => self.coordinator.protect(plan.range, protection),
            None if !self.coordinator.ledger().regions().iter().any(|region| {
                region.range().start() < plan.range.end() && plan.range.start() < region.range().end()
            }) =>
            {
                return LinuxResult::Value(0);
            }
            None => match &self.anonymous_charge {
                Some(lease) => {
                    let before = AnonymousMemoryLease::total(&self.coordinator.ledger().regions()).unwrap_or(u64::MAX);
                    let removed = Self::charged_overlap(&self.coordinator.ledger().regions(), plan.range);
                    lease
                        .transition(before.saturating_sub(removed), || self.coordinator.unmap(plan.range))
                        .map_err(|error| match error {
                            ChargeTransitionError::Limit => hl_memory::MemoryError::InvariantViolation,
                            ChargeTransitionError::Operation(error) => error,
                        })
                }
                None => self.coordinator.unmap(plan.range),
            },
        };
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(ErrorMap::ledger(error)),
        }
    }

    fn mremap(&self, arguments: [u64; 6]) -> LinuxResult {
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

    pub(super) fn charged_overlap(regions: &[hl_memory::Region], range: AddressRange) -> u64 {
        regions
            .iter()
            .filter_map(|region| region.charge())
            .map(|charge| {
                let first = charge.start().max(range.start());
                let last = charge.end().min(range.end());
                last.get().saturating_sub(first.get())
            })
            .sum()
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
        let added = reserved
            .then_some(plan.requested_new_length.saturating_sub(plan.requested_old_length))
            .unwrap_or(0);
        self.accounted(before.saturating_add(added), || {
            self.coordinator.apply(&batch).map(|_| old.start())
        })
    }

    pub(super) fn accounted(
        &self,
        target: u64,
        operation: impl FnOnce() -> Result<GuestAddress, hl_memory::MemoryError>,
    ) -> LinuxResult {
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

    fn mincore(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = MemoryAbi::new(&self.memory, self.architecture);
        let plan = match MemoryAbi::<M>::mincore_plan(arguments[0], arguments[1]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let Some(plan) = plan else {
            return LinuxResult::Value(0);
        };
        if !self.coordinator.contains(plan.range) {
            return LinuxResult::Error(Errno::ENOMEM);
        }
        if let Err(error) = abi.probe_mincore_output(arguments[2], plan.range.length()) {
            return LinuxResult::Error(ErrorMap::marshal(error));
        }
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let residency = match host.residency(plan) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::runtime(error)),
        };
        let expected = (plan.range.length() / 4096) as usize;
        if residency.len() != expected {
            return LinuxResult::Error(Errno::EIO);
        }
        let staged = match abi.stage_mincore(arguments[2], &residency) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(ErrorMap::marshal(error)),
        }
    }

    fn host_operation(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let result = match name {
            "madvise" => MemoryAbi::<M>::madvise(arguments[0], arguments[1], arguments[2] as i32)
                .map_err(ErrorMap::marshal)
                .and_then(|plan| host.advise(plan).map_err(ErrorMap::runtime)),
            "msync" => MemoryAbi::<M>::msync(arguments[0], arguments[1], arguments[2] as u32)
                .map_err(ErrorMap::marshal)
                .and_then(|plan| host.sync(plan).map_err(ErrorMap::runtime)),
            "mlock" | "mlock2" | "munlock" => Self::lock(host.as_ref(), name, arguments),
            "mlockall" => MemoryAbi::<M>::mlockall(arguments[0] as u32)
                .map_err(ErrorMap::marshal)
                .and_then(|plan| host.lock_all(plan).map_err(ErrorMap::runtime)),
            "munlockall" => host.unlock_all().map_err(ErrorMap::runtime),
            _ => return LinuxResult::Error(Errno::ENOSYS),
        };
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error),
        }
    }

    fn lock(host: &dyn RuntimeMemoryHost, name: &str, arguments: [u64; 6]) -> Result<(), Errno> {
        if name == "mlock" || name == "mlock2" {
            let (plan, on_fault) = if name == "mlock2" {
                (
                    MemoryAbi::<M>::mlock2(arguments[0], arguments[1], arguments[2] as u32)
                        .map_err(ErrorMap::marshal)?,
                    arguments[2] & 1 != 0,
                )
            } else {
                (
                    MemoryAbi::<M>::mlock(arguments[0], arguments[1]).map_err(ErrorMap::marshal)?,
                    false,
                )
            };
            host.lock(plan, on_fault).map_err(ErrorMap::runtime)
        } else {
            let plan = MemoryAbi::<M>::munlock(arguments[0], arguments[1]).map_err(ErrorMap::marshal)?;
            host.unlock(plan).map_err(ErrorMap::runtime)
        }
    }
}

impl<H: MappingHost, M: GuestMemory> MemorySyscalls for RuntimeMemorySyscalls<H, M> {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "brk" => self.brk.as_ref().map_or(LinuxResult::Error(Errno::ENOSYS), |brk| {
                let address = brk.set(arguments[0]);
                hl_log::hl_debug!(
                    hl_log::tag::MEMORY,
                    "brk requested={:#x} break={address:#x}",
                    arguments[0],
                );
                LinuxResult::Value(address)
            }),
            "mmap" => self.mmap(arguments, false),
            "mmap2" => self.mmap(arguments, true),
            "munmap" | "mprotect" => self.range_operation(operation.name, arguments),
            "mincore" => self.mincore(arguments),
            "madvise" | "msync" | "mlock" | "mlock2" | "munlock" | "mlockall" | "munlockall" => {
                self.host_operation(operation.name, arguments)
            }
            "mremap" => self.mremap(arguments),
            "get_mempolicy" => self.get_mempolicy(arguments[0]),
            "move_pages" => self.move_pages(arguments),
            "memfd_create" => self.memfd_create(arguments),
            "membarrier" => match arguments[0] {
                0 => LinuxResult::Value(0x7f),
                1 | 2 | 8 | 32 => {
                    std::sync::atomic::fence(Ordering::SeqCst);
                    LinuxResult::Value(0)
                }
                4 | 16 | 64 => LinuxResult::Value(0),
                _ => LinuxResult::Error(Errno::EINVAL),
            },
            "process_vm_readv" => self.process_vector(arguments, true),
            "process_vm_writev" => self.process_vector(arguments, false),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}

#[cfg(test)]
#[path = "syscalls_test.rs"]
mod tests;
