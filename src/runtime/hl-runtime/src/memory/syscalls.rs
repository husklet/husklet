use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_descriptor::DescriptorTable;
use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{
    Errno, GuestArchitecture, GuestMarshaller, GuestMemory, LinuxResult, MemoryAbi, MemorySyscalls, SyscallOperation,
};
use hl_memory::{MappingCoordinator, MappingHost, Protection, SharedObjectStore};

use crate::{BrkRegion, DescriptorMappingSource, MemfdRegistry, RuntimeMemoryHost};

use super::{AnonymousMemoryLease, charge::ChargeTransitionError, errno::ErrorMap};

mod mapping;
mod mremap;
mod vector;

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

    #[must_use]
    pub fn memfd_registry(&self) -> Arc<MemfdRegistry> {
        Arc::clone(&self.memfds)
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
