use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use super::space::AddressSpace;
use hl_isa::GuestAddress;
use hl_linux::{GuestAccess, GuestFault, GuestMemory};
use hl_memory::Protection;

/// Linux syscall access to the same arena used by guest execution.
#[derive(Clone)]
pub(super) struct ProcessMemory(Arc<AddressSpace>);

impl ProcessMemory {
    pub(super) fn new(memory: Arc<AddressSpace>) -> Self {
        Self(memory)
    }

    pub(super) fn lease(&self) -> super::space::SpaceLease {
        self.0.lease()
    }

    fn fault(address: u64, access: GuestAccess) -> GuestFault {
        GuestFault { address, access }
    }

    fn prefix(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let protection = match access {
            GuestAccess::Read => Protection::READ,
            GuestAccess::Write => Protection::WRITE,
        };
        let available = self
            .0
            .mappings()
            .access_prefix(GuestAddress::new(address), length as u64, protection)
            .map_err(|_| Self::fault(address, access))?;
        usize::try_from(available).map_err(|_| Self::fault(address, access))
    }
}

pub(super) struct ProcfsMemory {
    spaces: Arc<ProcfsSpaces>,
    paths: Option<super::path::MappingPaths>,
}

/// Instance-owned publication of live process address spaces for peer procfs
/// reads. Live address spaces remain weakly owned; exit retains only the final
/// value snapshot needed by zombie `/proc/<pid>/stat` until wait consumes it.
struct ProcfsSpace {
    live: Weak<AddressSpace>,
    retired_stat: Option<hl_runtime::ProcfsStatMetrics>,
}

pub(super) struct ProcfsSpaces(Mutex<BTreeMap<hl_task::ProcessId, ProcfsSpace>>);

impl ProcfsSpaces {
    pub(super) fn new(process: hl_task::ProcessId, space: &Arc<AddressSpace>) -> Arc<Self> {
        let spaces = Arc::new(Self(Mutex::new(BTreeMap::new())));
        spaces.publish(process, space);
        spaces
    }

    pub(super) fn publish(&self, process: hl_task::ProcessId, space: &Arc<AddressSpace>) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                process,
                ProcfsSpace {
                    live: Arc::downgrade(space),
                    retired_stat: None,
                },
            );
    }

    pub(super) fn capture_exit(self: &Arc<Self>, process: hl_task::ProcessId) -> Result<(), hl_runtime::ProcfsError> {
        let provider = ProcfsMemory::new(Arc::clone(self));
        let stat = hl_runtime::ProcfsStatPort::sample(&provider, process)?;
        let mut spaces = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let entry = spaces.get_mut(&process).ok_or(hl_runtime::ProcfsError::NotFound)?;
        entry.retired_stat = Some(stat);
        Ok(())
    }

    fn memory(&self, process: hl_task::ProcessId) -> Result<ProcessMemory, hl_runtime::ProcfsError> {
        let spaces = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let space = spaces
            .get(&process)
            .and_then(|entry| entry.live.upgrade())
            .ok_or(hl_runtime::ProcfsError::NotFound)?;
        Ok(ProcessMemory::new(space))
    }

    fn retired_stat(&self, process: hl_task::ProcessId) -> Option<hl_runtime::ProcfsStatMetrics> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&process)
            .and_then(|entry| entry.retired_stat)
    }

    #[cfg(test)]
    fn contains(&self, process: hl_task::ProcessId) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&process)
    }
}

impl hl_runtime::RuntimeReapPort for ProcfsSpaces {
    fn remove(&self, process: hl_task::ProcessId) {
        self.0.lock().unwrap_or_else(|error| error.into_inner()).remove(&process);
    }
}

impl ProcfsMemory {
    pub(super) fn new(spaces: Arc<ProcfsSpaces>) -> Self {
        Self {
            spaces,
            paths: None,
        }
    }

    pub(super) fn with_paths(mut self, paths: super::path::MappingPaths) -> Self {
        self.paths = Some(paths);
        self
    }
}

impl hl_runtime::ProcfsMemoryPort for ProcfsMemory {
    fn sample(&self, process: hl_task::ProcessId) -> Result<hl_runtime::ProcfsMemoryView, hl_runtime::ProcfsError> {
        self.address_space(process).map(|space| space.memory())
    }

    fn address_space(
        &self,
        process: hl_task::ProcessId,
    ) -> Result<hl_runtime::ProcfsAddressSpaceView, hl_runtime::ProcfsError> {
        let memory = self.spaces.memory(process)?;
        let snapshot = memory.0.mappings().snapshot();
        let provenance = memory.0.procfs_provenance();
        let regions = snapshot
            .regions
            .into_iter()
            .map(|region| {
                let (shared, device, inode, backing_identity) = match region.backing() {
                    hl_memory::Backing::Shared(_) => (true, 0, 0, None),
                    hl_memory::Backing::Anonymous { identity, shared } => (shared, 0, 0, Some(identity)),
                    hl_memory::Backing::File { identity, shared } => {
                        (shared, identity.device, identity.object, None)
                    }
                };
                let start = region.range().start().get();
                let end = region.range().end().get();
                let in_range = |range: Option<(u64, u64)>| {
                    range.is_some_and(|(lower, upper)| start < upper && lower < end)
                };
                let label = if in_range(provenance.stack_guard) {
                    Some(hl_runtime::ProcfsMemoryRegionLabel::StackGuard)
                } else if in_range(provenance.stack) {
                    Some(hl_runtime::ProcfsMemoryRegionLabel::Stack)
                } else if backing_identity == Some(hl_runtime::BRK_BACKING_IDENTITY) {
                    Some(hl_runtime::ProcfsMemoryRegionLabel::Heap)
                } else {
                    None
                };
                let path = self
                    .paths
                    .as_ref()
                    .and_then(|paths| (inode != 0).then(|| paths.path((device, inode))).flatten())
                    .or_else(|| {
                        provenance.executable.as_ref().and_then(|(lower, upper, path)| {
                            (start >= *lower && end <= *upper).then(|| path.clone())
                        })
                    });
                let pages = region.range().length().div_ceil(4096);
                hl_runtime::ProcfsMemoryRegionView {
                    start,
                    end,
                    protection: region.protection().bits(),
                    shared,
                    backing_offset: region.backing_offset(),
                    device,
                    inode,
                    path,
                    label,
                    resident_pages: if region.protection() == Protection::NONE {
                        0
                    } else {
                        pages
                    },
                }
            })
            .collect();
        hl_runtime::ProcfsAddressSpaceView::new(snapshot.generation, 4096, regions)
            .ok_or(hl_runtime::ProcfsError::Invalid)
    }

    fn environment(&self, process: hl_task::ProcessId) -> Result<Vec<u8>, hl_runtime::ProcfsError> {
        let environment = self.spaces.memory(process)?.0.procfs_provenance().environment;
        let capacity = environment.iter().map(|entry| entry.len() + 1).sum();
        let mut bytes = Vec::with_capacity(capacity);
        for entry in environment {
            bytes.extend_from_slice(&entry);
            bytes.push(0);
        }
        Ok(bytes)
    }
}

impl hl_runtime::ProcfsStatPort for ProcfsMemory {
    fn sample(&self, process: hl_task::ProcessId) -> Result<hl_runtime::ProcfsStatMetrics, hl_runtime::ProcfsError> {
        let space = match hl_runtime::ProcfsMemoryPort::address_space(self, process) {
            Ok(space) => Some(space),
            Err(hl_runtime::ProcfsError::NotFound) => {
                return self.spaces.retired_stat(process).ok_or(hl_runtime::ProcfsError::NotFound);
            }
            Err(error) => return Err(error),
        };
        let memory = space.as_ref().map(hl_runtime::ProcfsAddressSpaceView::memory);
        let virtual_bytes = match memory {
            Some(memory) => memory
                .total_pages
                .checked_mul(memory.page_bytes)
                .ok_or(hl_runtime::ProcfsError::Invalid)?,
            None => 128 << 20,
        };
        let resident_pages = memory.map_or(0, |memory| i64::try_from(memory.resident_pages).unwrap_or(i64::MAX));
        let executable = space.as_ref().and_then(|space| {
            space
                .regions
                .iter()
                .find(|region| region.protection & Protection::EXECUTE.bits() != 0)
        });
        let writable = space.as_ref().and_then(|space| {
            space
                .regions
                .iter()
                .find(|region| region.protection & Protection::WRITE.bits() != 0)
        });
        let stack = space.as_ref().and_then(|space| {
            space
                .regions
                .iter()
                .find(|region| region.label == Some(hl_runtime::ProcfsMemoryRegionLabel::Stack))
        });
        Ok(hl_runtime::ProcfsStatMetrics {
            terminal: 0,
            flags: 4_194_560,
            minor_faults: 0,
            child_minor_faults: 0,
            major_faults: 0,
            child_major_faults: 0,
            user_ticks: 0,
            system_ticks: 0,
            child_user_ticks: 0,
            child_system_ticks: 0,
            priority: 20,
            nice: 0,
            interval_ticks: 0,
            start_ticks: 0,
            virtual_bytes,
            resident_pages,
            resident_limit: u64::MAX,
            code_start: executable.map_or(0, |region| region.start),
            code_end: executable.map_or(0, |region| region.end),
            stack_start: stack.map_or(0, |region| region.end),
            stack_pointer: 0,
            instruction_pointer: 0,
            wait_channel: 0,
            swapped_pages: 0,
            child_swapped_pages: 0,
            exit_signal: 17,
            processor: 0,
            realtime_priority: 0,
            policy: 0,
            delay_ticks: 0,
            guest_ticks: 0,
            child_guest_ticks: 0,
            data_start: writable.map_or(0, |region| region.start),
            data_end: writable.map_or(0, |region| region.end),
            heap_start: space.as_ref().and_then(|space| {
                space
                    .regions
                    .iter()
                    .find(|region| region.label == Some(hl_runtime::ProcfsMemoryRegionLabel::Heap))
            }).map_or(0, |region| region.start),
            arguments_start: 0,
            arguments_end: 0,
            environment_start: 0,
            environment_end: 0,
        })
    }
}

impl GuestMemory for ProcessMemory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let mut available = 0_usize;
        while available < length {
            let current = address
                .checked_add(available as u64)
                .ok_or_else(|| Self::fault(u64::MAX, access))?;
            match self.prefix(current, length - available, access) {
                Ok(0) => return Err(Self::fault(current, access)),
                Ok(span) => available += span.min(length - available),
                Err(_) if available != 0 => return Ok(available),
                Err(fault) => return Err(fault),
            }
        }
        Ok(available)
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        if destination.is_empty() {
            return Ok(0);
        }
        let available = self.prefix(address, destination.len(), GuestAccess::Read)?;
        self.0
            .mappings()
            .read(
                GuestAddress::new(address),
                &mut destination[..available],
                Protection::READ,
            )
            .map(|()| available)
            .map_err(|_| Self::fault(address, GuestAccess::Read))
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        if source.is_empty() {
            return Ok(0);
        }
        let mappings = self.0.mappings();
        let available = self.prefix(address, source.len(), GuestAccess::Write)?;
        mappings
            .prepare_write(GuestAddress::new(address), available as u64)
            .and_then(|write| mappings.commit_write(write, &source[..available]))
            .map(|_| available)
            .map_err(|_| Self::fault(address, GuestAccess::Write))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use hl_isa::{GuestAddress, GuestArchitecture};
    use hl_linux::{GuestAccess, GuestMarshaller, GuestMemory};
    use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement, Protection};

    use super::{AddressSpace, ProcessMemory, ProcfsMemory, ProcfsSpaces};
    use crate::ffi::linux::{MappingHostAdapter, VirtualMemory};

    const PAGE: usize = 4096;

    fn memory() -> (ProcessMemory, Arc<VirtualMemory>) {
        let arena = Arc::new(VirtualMemory::reserve(PAGE * 2).unwrap());
        let mappings = Arc::new(MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena))));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: PAGE as u64,
                alignment: PAGE as u64,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(PAGE as u64)),
                length: PAGE as u64,
                alignment: PAGE as u64,
                protection: Protection::NONE,
                backing: Backing::Anonymous {
                    identity: 2,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        let space = AddressSpace::new(Arc::clone(&arena), mappings);
        (ProcessMemory::new(space), arena)
    }

    #[test]
    fn procfs_stat_uses_guest_mapping_accounting() {
        let (memory, _arena) = memory();
        let process = hl_task::ProcessId::from_wire(7, 1).unwrap();
        let spaces = ProcfsSpaces::new(process, &memory.0);
        let provider = ProcfsMemory::new(spaces);
        let metrics = hl_runtime::ProcfsStatPort::sample(&provider, process).unwrap();

        assert_eq!(metrics.virtual_bytes, (PAGE * 2) as u64);
        assert_eq!(metrics.resident_pages, 1);
        assert_eq!(metrics.resident_limit, u64::MAX);
        assert_eq!(metrics.priority, 20);
        assert_eq!(metrics.exit_signal, 17);

        let peer = hl_task::ProcessId::from_wire(8, 1).unwrap();
        assert_eq!(
            hl_runtime::ProcfsStatPort::sample(&provider, peer),
            Err(hl_runtime::ProcfsError::NotFound)
        );
    }

    #[test]
    fn procfs_metrics_follow_fork_exec_exit_and_reap() {
        let (parent_memory, _parent_arena) = memory();
        let parent = hl_task::ProcessId::from_wire(7, 1).unwrap();
        let peer = hl_task::ProcessId::from_wire(8, 1).unwrap();
        let spaces = ProcfsSpaces::new(parent, &parent_memory.0);
        let provider = ProcfsMemory::new(Arc::clone(&spaces));

        let (peer_memory, _peer_arena) = memory();
        spaces.publish(peer, &peer_memory.0);
        assert_eq!(
            hl_runtime::ProcfsMemoryPort::address_space(&provider, peer)
                .unwrap()
                .memory()
                .total_pages,
            2
        );
        let live_stat = hl_runtime::ProcfsStatPort::sample(&provider, peer).unwrap();
        peer_memory
            .0
            .mappings()
            .unmap(
                hl_isa::AddressRange::nonempty(GuestAddress::new(PAGE as u64), PAGE as u64)
                    .unwrap(),
            )
            .unwrap();
        spaces.capture_exit(peer).unwrap();
        let fork_exit = hl_runtime::ProcfsStatPort::sample(&provider, peer).unwrap();
        assert_ne!(fork_exit, live_stat);
        assert_eq!(fork_exit.virtual_bytes, PAGE as u64);

        drop(peer_memory);
        assert_eq!(
            hl_runtime::ProcfsMemoryPort::address_space(&provider, peer),
            Err(hl_runtime::ProcfsError::NotFound)
        );
        assert_eq!(hl_runtime::ProcfsStatPort::sample(&provider, peer), Ok(fork_exit));

        let (exec_memory, _exec_arena) = memory();
        spaces.publish(peer, &exec_memory.0);
        assert_eq!(
            hl_runtime::ProcfsStatPort::sample(&provider, peer)
                .unwrap()
                .virtual_bytes,
            (PAGE * 2) as u64
        );
        spaces.capture_exit(peer).unwrap();
        let exec_exit = hl_runtime::ProcfsStatPort::sample(&provider, peer).unwrap();
        drop(exec_memory);
        assert_eq!(hl_runtime::ProcfsStatPort::sample(&provider, peer), Ok(exec_exit));
        assert!(spaces.contains(peer));
        hl_runtime::RuntimeReapPort::remove(spaces.as_ref(), peer);
        assert!(!spaces.contains(peer));
        assert_eq!(
            hl_runtime::ProcfsStatPort::sample(&provider, peer),
            Err(hl_runtime::ProcfsError::NotFound)
        );
    }

    #[test]
    fn read_partial_fault() {
        let (memory, arena) = memory();
        let expected = vec![0x5a; PAGE];
        arena.write(0, &expected).unwrap();
        let mut output = vec![0; PAGE * 2];

        let progress = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64).copy_from(0, &mut output);

        assert_eq!(progress.copied, PAGE);
        assert_eq!(progress.fault.unwrap().address, PAGE as u64);
        assert_eq!(&output[..PAGE], expected);
        assert_eq!(memory.probe(0, PAGE * 2, GuestAccess::Read), Ok(PAGE));
    }

    #[test]
    fn write_partial_fault() {
        let (memory, arena) = memory();
        let input = vec![0xa5; PAGE * 2];

        let progress = GuestMarshaller::new(&memory, GuestArchitecture::X86_64).copy_to(0, &input);

        assert_eq!(progress.copied, PAGE);
        assert_eq!(progress.fault.unwrap().address, PAGE as u64);
        let mut observed = vec![0; PAGE];
        arena.read(0, &mut observed).unwrap();
        assert_eq!(observed, input[..PAGE]);
        assert_eq!(memory.probe(0, PAGE * 2, GuestAccess::Write), Ok(PAGE));
    }

    #[test]
    fn file_bus_boundary() {
        const FILE_LENGTH: usize = 16 * 1024;
        let path = std::env::temp_dir().join(format!("hl-guest-prefix-{}", std::process::id(),));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        fs::remove_file(path).unwrap();
        file.set_len(FILE_LENGTH as u64).unwrap();
        let identity = hl_memory::FileIdentity { device: 9, object: 17 };
        let arena = Arc::new(VirtualMemory::reserve(FILE_LENGTH * 2).unwrap());
        arena.register_file(identity, &file).unwrap();
        let mappings = Arc::new(MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena))));
        mappings
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: (FILE_LENGTH * 2) as u64,
                alignment: PAGE as u64,
                protection: Protection::READ,
                backing: Backing::File {
                    identity,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        let memory = ProcessMemory::new(AddressSpace::new(arena, mappings));
        let mut output = vec![0; FILE_LENGTH * 2];

        let progress = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64).copy_from(0, &mut output);

        assert_eq!(progress.copied, FILE_LENGTH);
        assert_eq!(progress.fault.unwrap().address, FILE_LENGTH as u64);
    }
}
