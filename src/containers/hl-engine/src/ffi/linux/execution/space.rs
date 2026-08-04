use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use hl_execution::{MemoryProjection, ProjectionControl};
use hl_memory::MappingCoordinator;
use hl_memory::{AddressSpaceId, MapRequest, Placement, Protection, Region};

use super::super::virtual_advice::ForkAdvice;
use super::process_memory::ProcessMemory;
use super::{ArenaMemory, MappingHostAdapter, VirtualMemory, operand::SliceMemory};

/// Complete memory identity owned by one guest process address space.
pub(super) struct AddressSpace {
    current: RwLock<Current>,
    projection: Arc<ProcessProjection>,
    procfs: RwLock<ProcfsProvenance>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ProcfsProvenance {
    pub(super) executable: Option<(u64, u64, Vec<u8>)>,
    pub(super) environment: Vec<Vec<u8>>,
    pub(super) stack: Option<(u64, u64)>,
    pub(super) stack_guard: Option<(u64, u64)>,
}

#[derive(Debug, Default)]
struct ProjectionEffects {
    epoch: AtomicU64,
    transitions: AtomicU64,
    beginnings: AtomicU64,
    endings: AtomicU64,
}

#[derive(Clone, Debug)]
struct ProjectionEffectsControl(Arc<ProjectionEffects>);

impl ProjectionControl for ProjectionEffectsControl {
    fn activate(&self) {
        self.0.epoch.fetch_add(1, Ordering::AcqRel);
    }
    fn begin_transition(&self) {
        self.0.beginnings.fetch_add(1, Ordering::AcqRel);
        self.0.transitions.fetch_add(1, Ordering::AcqRel);
    }
    fn end_transition(&self) {
        self.0.transitions.fetch_sub(1, Ordering::AcqRel);
        self.0.endings.fetch_add(1, Ordering::AcqRel);
    }
}

type ProjectionQuery = Box<dyn Fn(u64, u64) -> u64 + Send + Sync>;
type ProcessMemoryProjection = MemoryProjection<ProjectionQuery, ProjectionEffectsControl>;

struct ProcessProjection {
    projection: ProcessMemoryProjection,
    effects: Arc<ProjectionEffects>,
    target: Arc<RwLock<Arc<VirtualMemory>>>,
}

impl ProcessProjection {
    fn new(arena: Arc<VirtualMemory>, generation: u64) -> Arc<Self> {
        let effects = Arc::new(ProjectionEffects::default());
        let target = Arc::new(RwLock::new(arena));
        let mut projection = MemoryProjection::new(ProjectionEffectsControl(Arc::clone(&effects)));
        let query_target = Arc::clone(&target);
        let query: ProjectionQuery = Box::new(move |storage, length| {
            let arena = query_target.read().unwrap_or_else(|error| error.into_inner());
            let Some(guest) = arena.guest_address(storage) else {
                return 0;
            };
            let Some(fault) = arena.bus_fault(guest, length) else {
                return 0;
            };
            arena.storage_address(fault, 1).unwrap_or(0)
        });
        projection.install(query, true, generation);
        Arc::new(Self {
            projection,
            effects,
            target,
        })
    }

    fn epoch(&self) -> u64 {
        self.effects.epoch.load(Ordering::Acquire)
    }
    fn begin(&self) {
        self.projection.begin_transition();
    }
    fn end(&self) {
        self.projection.end_transition();
    }

    fn rebind(&self, arena: Arc<VirtualMemory>) {
        *self.target.write().unwrap_or_else(|error| error.into_inner()) = arena;
        self.effects.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct ProjectionObserver {
    projection: Arc<ProcessProjection>,
    incarnation: u64,
}

impl hl_memory::MappingTransitionObserver for ProjectionObserver {
    fn begin(&self) {
        self.projection.begin();
    }
    fn published(&self, generation: u64) {
        self.projection
            .projection
            .observe((self.incarnation << 32) | (generation & u64::from(u32::MAX)), true);
        self.projection.effects.epoch.fetch_add(1, Ordering::AcqRel);
    }
    fn end(&self) {
        self.projection.end();
    }
}

impl std::fmt::Debug for ProcessProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessProjection")
            .field("epoch", &self.epoch())
            .finish()
    }
}

struct Current {
    generation: u64,
    image: Arc<SpaceImage>,
}

struct SpaceImage {
    arena: Arc<VirtualMemory>,
    mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
}

#[derive(Clone)]
pub(super) struct SpaceLease {
    generation: u64,
    image: Arc<SpaceImage>,
}

impl SpaceLease {
    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn arena(&self) -> Arc<VirtualMemory> {
        Arc::clone(&self.image.arena)
    }
    pub(super) fn mappings(&self) -> Arc<MappingCoordinator<MappingHostAdapter>> {
        Arc::clone(&self.image.mappings)
    }
    pub(super) fn mappings_ref(&self) -> &MappingCoordinator<MappingHostAdapter> {
        &self.image.mappings
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Capacity,
    Memory,
    Host,
}

impl AddressSpace {
    pub(super) fn exec_space(&self, identity: AddressSpaceId) -> Result<Arc<Self>, Error> {
        let current = self.lease();
        let shared = current.image.mappings.shared_objects().ok_or(Error::Memory)?;
        let arena = Arc::new(
            VirtualMemory::reserve_in(current.image.arena.resource_context(), current.image.arena.length())
                .map_err(|_| Error::Memory)?
                .with_shared_store(Arc::clone(&shared))
                .with_shared_backings(current.image.arena.shared_backings.clone().ok_or(Error::Memory)?)
                .with_file_registry(current.image.arena.file_registry()),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            identity,
        ));
        Ok(Self::new(arena, mappings))
    }

    fn copy_length(
        &self,
        source: &VirtualMemory,
        backing: hl_memory::Backing,
        backing_offset: u64,
        length: u64,
        wipe: bool,
    ) -> Result<u64, Error> {
        if wipe {
            return Ok(0);
        }
        match backing {
            hl_memory::Backing::File { shared: true, .. } => Ok(0),
            hl_memory::Backing::File {
                identity,
                shared: false,
            } => {
                let valid = source.file_valid_length(identity).map_err(|_| Error::Host)?;
                Ok(valid.saturating_sub(backing_offset).min(length))
            }
            _ => Ok(length),
        }
    }

    fn copy_segment(
        &self,
        authority: &hl_memory::FrozenSnapshotAuthority,
        source: &VirtualMemory,
        arena: &VirtualMemory,
        mappings: &MappingCoordinator<MappingHostAdapter>,
        region: Region,
        range: hl_isa::AddressRange,
        advice: Option<ForkAdvice>,
    ) -> Result<(), Error> {
        const CHUNK: usize = 1024 * 1024;
        if advice == Some(ForkAdvice::Omit) {
            return Ok(());
        }
        let region_offset = range
            .start()
            .get()
            .checked_sub(region.range().start().get())
            .ok_or(Error::Capacity)?;
        let backing_offset = region
            .backing_offset()
            .checked_add(region_offset)
            .ok_or(Error::Capacity)?;
        let backing = if advice == Some(ForkAdvice::Wipe) {
            hl_memory::Backing::Anonymous {
                identity: 0x5749_5045_0000_0000 ^ range.start().get(),
                shared: false,
            }
        } else {
            region.backing()
        };
        mappings
            .map_inherited(MapRequest {
                placement: Placement::Fixed(range.start()),
                length: range.length(),
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing,
                backing_offset,
            })
            .map_err(|_| Error::Memory)?;
        let copy_length = self.copy_length(
            source,
            backing,
            backing_offset,
            range.length(),
            advice == Some(ForkAdvice::Wipe),
        )?;
        let mut offset = 0_u64;
        while offset < copy_length {
            let length = usize::try_from((copy_length - offset).min(CHUNK as u64)).map_err(|_| Error::Capacity)?;
            let mut buffer = vec![0; length];
            let address = range.start().get().checked_add(offset).ok_or(Error::Capacity)?;
            source
                .frozen_snapshot_read(authority, address, &mut buffer, region.protection())
                .map_err(|_| Error::Host)?;
            arena.write(address, &buffer).map_err(|_| Error::Host)?;
            offset += length as u64;
        }
        if region.protection() != Protection::READ.union(Protection::WRITE) {
            mappings
                .protect(range, region.protection())
                .map_err(|_| Error::Memory)?;
        }
        if advice == Some(ForkAdvice::Wipe) {
            arena.update_advice(range, advice).map_err(|_| Error::Memory)?;
        }
        Ok(())
    }

    pub(super) fn new(arena: Arc<VirtualMemory>, mappings: Arc<MappingCoordinator<MappingHostAdapter>>) -> Arc<Self> {
        let generation = 1_u64;
        let projection = ProcessProjection::new(Arc::clone(&arena), generation << 32);
        mappings.set_transition_observer(Arc::new(ProjectionObserver {
            projection: Arc::clone(&projection),
            incarnation: generation,
        }));
        Arc::new(Self {
            current: RwLock::new(Current {
                generation: 1,
                image: Arc::new(SpaceImage { arena, mappings }),
            }),
            projection,
            procfs: RwLock::new(ProcfsProvenance::default()),
        })
    }

    pub(super) fn publish_procfs_image(
        &self,
        loaded: &hl_loader::LoadedProcess,
        executable: Vec<u8>,
        environment: Vec<Vec<u8>>,
    ) {
        let main = loaded.main();
        let stack = loaded.usable_stack();
        let mapping = loaded.stack_mapping();
        let guard_end = stack.address();
        *self.procfs.write().unwrap_or_else(|error| error.into_inner()) = ProcfsProvenance {
            executable: Some((main.address(), main.address().saturating_add(main.size()), executable)),
            environment,
            stack: Some((stack.address(), mapping.address().saturating_add(mapping.size()))),
            stack_guard: (mapping.address() < guard_end).then_some((mapping.address(), guard_end)),
        };
    }

    pub(super) fn procfs_provenance(&self) -> ProcfsProvenance {
        self.procfs.read().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub(super) fn lease(&self) -> SpaceLease {
        let current = self.current.read().unwrap_or_else(|error| error.into_inner());
        SpaceLease {
            generation: current.generation,
            image: Arc::clone(&current.image),
        }
    }

    pub(super) fn replace(
        &self,
        expected: u64,
        arena: Arc<VirtualMemory>,
        mappings: Arc<MappingCoordinator<MappingHostAdapter>>,
    ) -> Result<SpaceLease, Error> {
        let mut current = self.current.write().unwrap_or_else(|error| error.into_inner());
        if current.generation != expected {
            return Err(Error::Host);
        }
        let previous = SpaceLease {
            generation: current.generation,
            image: Arc::clone(&current.image),
        };
        current.generation = current.generation.checked_add(1).ok_or(Error::Capacity)?;
        self.projection.rebind(Arc::clone(&arena));
        mappings.set_transition_observer(Arc::new(ProjectionObserver {
            projection: Arc::clone(&self.projection),
            incarnation: current.generation,
        }));
        current.image = Arc::new(SpaceImage { arena, mappings });
        Ok(previous)
    }

    pub(super) fn arena(&self) -> Arc<VirtualMemory> {
        let lease = self.lease();
        debug_assert_ne!(lease.generation, 0);
        lease.arena()
    }

    pub(super) fn mappings(&self) -> Arc<MappingCoordinator<MappingHostAdapter>> {
        self.lease().mappings()
    }

    #[cfg(test)]
    pub(super) fn projection_epoch(&self) -> u64 {
        self.projection.epoch()
    }

    pub(super) fn resolve_bus(&self, lease: &SpaceLease, address: u64, length: u64) -> Option<u64> {
        let storage = lease.arena().storage_address(address, length)?;
        let resolved = self.projection.projection.resolve(storage, length);
        if resolved == 0 {
            return None;
        }
        lease.arena().guest_address(resolved)
    }

    pub(super) fn guest_memory(self: &Arc<Self>) -> ProcessMemory {
        ProcessMemory::new(Arc::clone(self))
    }

    pub(super) fn arena_memory(self: &Arc<Self>) -> ArenaMemory {
        ArenaMemory {
            space: Arc::clone(self),
        }
    }

    pub(super) fn with_execution_memory<R>(self: &Arc<Self>, callback: impl FnOnce(&mut SliceMemory<'_>) -> R) -> R {
        let current = self.current.read().unwrap_or_else(|error| error.into_inner());
        let lease = SpaceLease {
            generation: current.generation,
            image: Arc::clone(&current.image),
        };
        let mut memory = SliceMemory {
            space: self,
            lease: &lease,
        };
        callback(&mut memory)
    }

    pub(super) fn fork_snapshot(&self, identity: AddressSpaceId) -> Result<Arc<Self>, Error> {
        let current = self.lease();
        self.fork_bounded(identity, current.image.arena.length() as u64)
    }

    fn fork_bounded(&self, identity: AddressSpaceId, byte_limit: u64) -> Result<Arc<Self>, Error> {
        let current = self.lease();
        current.image.mappings.with_frozen_snapshot(|authority, snapshot| {
            self.fork_frozen(&current, authority, snapshot, identity, byte_limit)
        })
    }

    fn fork_frozen(
        &self,
        current: &SpaceLease,
        authority: &hl_memory::FrozenSnapshotAuthority,
        snapshot: hl_memory::MemoryLedgerSnapshot,
        identity: AddressSpaceId,
        byte_limit: u64,
    ) -> Result<Arc<Self>, Error> {
        const REGION_LIMIT: usize = 4096;
        let bytes = snapshot.regions.iter().try_fold(0_u64, |total, region| {
            total.checked_add(region.range().length()).ok_or(Error::Capacity)
        })?;
        if snapshot.regions.len() > REGION_LIMIT || bytes > byte_limit {
            return Err(Error::Capacity);
        }
        let shared = current.image.mappings.shared_objects().ok_or(Error::Memory)?;
        let arena = Arc::new(
            VirtualMemory::reserve_in(current.image.arena.resource_context(), current.image.arena.length())
                .map_err(|_| Error::Memory)?
                .with_inherited_store(Arc::clone(&shared))
                .with_inherited_backings(&current.image.arena)
                .with_file_registry(current.image.arena.file_registry()),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            identity,
        ));
        for region in snapshot.regions {
            let segments = current
                .image
                .arena
                .advice_segments(region.range())
                .map_err(|_| Error::Memory)?;
            for (range, advice) in segments {
                self.copy_segment(
                    authority,
                    &current.image.arena,
                    &arena,
                    &mappings,
                    region,
                    range,
                    advice,
                )?;
            }
        }
        let forked = Self::new(arena, mappings);
        *forked.procfs.write().unwrap_or_else(|error| error.into_inner()) = self.procfs_provenance();
        Ok(forked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, StatusFlags};
    use hl_execution::{
        Aarch64CpuState, CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionInstructionMemory,
        ExecutionMachine, ExecutionSnapshot, GuestOperandMemory, StepOutcome,
    };
    use hl_isa::GuestAddress;
    use hl_linux::{DescriptorIoSyscalls, GuestMarshaller, LinuxResult, SyscallFamily, SyscallOperation};
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    fn space() -> (Arc<AddressSpace>, Arc<hl_memory::SharedObjectStore>) {
        let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
        let arena = Arc::new(
            VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            Arc::clone(&shared),
            AddressSpaceId { slot: 1, generation: 1 },
        ));
        (AddressSpace::new(arena, mappings), shared)
    }

    #[test]
    fn publication_observed() {
        let (space, _) = space();
        let memory = space.arena_memory();
        let before = memory.instruction_epoch().unwrap();
        let beginnings = space.projection.effects.beginnings.load(Ordering::Acquire);
        let endings = space.projection.effects.endings.load(Ordering::Acquire);

        space
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::EXECUTE),
                hl_memory::Backing::Anonymous {
                    identity: 91,
                    shared: false,
                },
            ))
            .unwrap();

        let after = memory.instruction_epoch().unwrap();
        assert_ne!(after, before);
        assert_eq!(
            space.projection.effects.beginnings.load(Ordering::Acquire),
            beginnings + 1,
        );
        assert_eq!(space.projection.effects.endings.load(Ordering::Acquire), endings + 1,);
        assert_eq!(space.projection.effects.transitions.load(Ordering::Acquire), 0);
    }

    #[test]
    fn native_projection_tracks_permissions_storage_and_smc() {
        let (space, _) = space();
        let protection = Protection::READ.union(Protection::WRITE).union(Protection::EXECUTE);
        space
            .mappings()
            .map(request(
                0,
                protection,
                hl_memory::Backing::Anonymous {
                    identity: 92,
                    shared: false,
                },
            ))
            .unwrap();
        let expected = space.arena().storage_address(0, 16).unwrap();
        let mappings = space.mappings();
        let lease = mappings
            .project_contiguous(GuestAddress::new(0), 16, Protection::READ, space.lease().generation())
            .unwrap();
        assert_eq!(lease.storage_address(), expected);
        assert_eq!(lease.protection(), protection);
        drop(lease);

        let before = space.arena_memory().instruction_epoch().unwrap();
        let writable = mappings
            .project_contiguous(GuestAddress::new(0), 16, Protection::WRITE, space.lease().generation())
            .unwrap();
        space.arena().write_untracked(0, &[1; 16]).unwrap();
        writable.publish_written().unwrap();
        let after = space.arena_memory().instruction_epoch().unwrap();
        assert!(after.writes > before.writes);

        mappings
            .protect(
                hl_isa::AddressRange::nonempty(GuestAddress::new(0), 4096).unwrap(),
                Protection::READ.union(Protection::EXECUTE),
            )
            .unwrap();
        assert!(
            mappings
                .project_contiguous(GuestAddress::new(0), 16, Protection::WRITE, space.lease().generation())
                .is_err()
        );
    }

    #[test]
    fn native_projection_reconciles_shared_backing() {
        let (space, shared) = space();
        let object = shared.create(1, 4096).unwrap();
        space
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Shared(hl_memory::SharedBackingRef {
                    object,
                    offset: 0,
                    length: 4096,
                    write_shared: true,
                }),
            ))
            .unwrap();
        let mappings = space.mappings();
        let lease = mappings
            .project_contiguous(GuestAddress::new(0), 4, Protection::WRITE, space.lease().generation())
            .unwrap();
        space.arena().write_untracked(0, b"rust").unwrap();
        lease.publish_written().unwrap();
        let pin = shared.pin(object, false).unwrap();
        let mut bytes = [0_u8; 4];
        pin.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"rust");
    }

    #[test]
    fn crosses_exec_regions() {
        let (space, _) = space();
        for (address, identity) in [(0, 81), (4096, 82)] {
            space
                .mappings()
                .map(request(
                    address,
                    Protection::EXECUTE,
                    hl_memory::Backing::Anonymous {
                        identity,
                        shared: false,
                    },
                ))
                .unwrap();
        }
        let memory = space.arena_memory();
        let mut bytes = [0xa5_u8; 4];

        assert_eq!(memory.fetch(4094, &mut bytes), Ok(4));
        assert_eq!(bytes, [0; 4]);

        space
            .mappings()
            .protect(
                hl_isa::AddressRange::nonempty(GuestAddress::new(4096), 4096).unwrap(),
                Protection::NONE,
            )
            .unwrap();
        bytes.fill(0xa5);
        assert_eq!(memory.fetch(4094, &mut bytes), Err(()));
        assert_eq!(bytes, [0xa5; 4]);
    }

    #[test]
    fn fetch_tracks_mappings() {
        let (space, _) = space();
        let identity = hl_memory::FileIdentity { device: 51, object: 52 };
        let path = std::env::temp_dir().join(format!("hl-fetch-mappings-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut source = [0_u8; 8192];
        for (index, byte) in source.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(29).wrapping_add(7);
        }
        file.write_all(&source).unwrap();
        file.flush().unwrap();
        space.arena().register_file(identity, &file).unwrap();
        let executable = MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length: 8192,
            alignment: 4096,
            protection: Protection::EXECUTE,
            backing: hl_memory::Backing::File { identity, shared: true },
            backing_offset: 0,
        };
        space.mappings().map(executable).unwrap();
        let memory = space.arena_memory();

        let mut crossing = [0_u8; 15];
        assert_eq!(memory.fetch(4090, &mut crossing), Ok(15));
        assert_eq!(crossing, source[4090..4105]);

        let first_rewrite = std::array::from_fn::<_, 15, _>(|index| (index as u8).wrapping_mul(53).wrapping_add(11));
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&first_rewrite).unwrap();
        file.flush().unwrap();
        let mut fetched = [0_u8; 15];
        assert_eq!(memory.fetch(0, &mut fetched), Ok(15));
        assert_eq!(fetched, first_rewrite);

        space
            .mappings()
            .unmap(hl_isa::AddressRange::nonempty(GuestAddress::new(4096), 4096).unwrap())
            .unwrap();
        let mut prefix = [0_u8; 2];
        assert_eq!(memory.fetch(4094, &mut prefix), Ok(2));
        assert_eq!(prefix, source[4094..4096]);

        space
            .mappings()
            .unmap(hl_isa::AddressRange::nonempty(GuestAddress::new(0), 4096).unwrap())
            .unwrap();
        fetched.fill(0xa5);
        assert_eq!(memory.fetch(0, &mut fetched), Err(()));
        assert_eq!(fetched, [0xa5; 15]);

        let second_rewrite = std::array::from_fn::<_, 15, _>(|index| (index as u8).wrapping_mul(17).wrapping_add(3));
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&second_rewrite).unwrap();
        file.flush().unwrap();
        let before = memory.instruction_epoch().unwrap();
        let mut replacement = executable;
        replacement.length = 4096;
        space.mappings().map(replacement).unwrap();
        let after = memory.instruction_epoch().unwrap();
        assert_ne!(after, before);
        assert_eq!(memory.fetch(0, &mut fetched), Ok(15));
        assert_eq!(fetched, second_rewrite);

        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn bus_projection_rebinds() {
        let (space, shared) = space();
        let identity = hl_memory::FileIdentity { device: 41, object: 42 };
        let path = std::env::temp_dir().join(format!("hl-bus-projection-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(1).unwrap();
        space.arena().register_file(identity, &file).unwrap();
        assert!(space.arena().validate_file(identity, 4096, 8, 4096).is_err());
        let lease = space.lease();
        assert_eq!(space.resolve_bus(&lease, 4096, 8), Some(4096));
        assert!(lease.arena().take_bus(4096));
        assert_eq!(space.resolve_bus(&lease, 4096, 8), None);

        let arena = Arc::new(
            VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            AddressSpaceId { slot: 1, generation: 2 },
        ));
        let before = space.projection_epoch();
        space.replace(1, arena, mappings).unwrap();
        assert!(space.projection_epoch() > before);
        assert_eq!(space.resolve_bus(&space.lease(), 4096, 8), None);

        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    fn request(start: u64, protection: Protection, backing: hl_memory::Backing) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(start)),
            length: 4096,
            alignment: 4096,
            protection,
            backing,
            backing_offset: 0,
        }
    }

    fn replacement(
        shared: Arc<hl_memory::SharedObjectStore>,
        generation: u64,
        value: u8,
    ) -> (Arc<VirtualMemory>, Arc<MappingCoordinator<MappingHostAdapter>>) {
        let arena = Arc::new(
            VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            AddressSpaceId { slot: 1, generation },
        ));
        mappings
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: generation,
                    shared: false,
                },
            ))
            .unwrap();
        arena.write(0, &[value]).unwrap();
        (arena, mappings)
    }

    #[test]
    fn dynamic_replace() {
        let (space, shared) = space();
        let memory = space.arena_memory();
        let before = memory.instruction_epoch().unwrap();
        let (arena, mappings) = replacement(shared, 2, 0x5a);

        space.replace(1, arena, mappings).unwrap();

        assert_eq!(memory.read(0, 1), Ok(0x5a));
        let after = memory.instruction_epoch().unwrap();
        assert_ne!(after.mappings, before.mappings);
    }

    #[test]
    fn guarded_replace() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (space, shared) = space();
        let (arena, mappings) = replacement(shared, 2, 0x6b);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (replaced_tx, replaced_rx) = mpsc::channel();
        let guarded = Arc::clone(&space);
        let callback = std::thread::spawn(move || {
            guarded.with_execution_memory(|memory| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                assert_eq!(memory.read(0, 1), Err(()));
            });
        });
        started_rx.recv().unwrap();
        let replacing = Arc::clone(&space);
        let replace = std::thread::spawn(move || {
            replacing.replace(1, arena, mappings).unwrap();
            replaced_tx.send(()).unwrap();
        });

        assert!(replaced_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        replaced_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        callback.join().unwrap();
        replace.join().unwrap();
        space.with_execution_memory(|memory| assert_eq!(memory.read(0, 1), Ok(0x6b)));
    }

    fn machine(architecture: hl_isa::GuestArchitecture, pc: u64) -> ExecutionMachine {
        let cpu = match architecture {
            hl_isa::GuestArchitecture::Aarch64 => ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
                pc,
                ..Aarch64CpuState::default()
            }),
            hl_isa::GuestArchitecture::X86_64 => ExecutionCpuSnapshot::X86_64(CpuState {
                rip: pc,
                ..CpuState::default()
            }),
        };
        ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        })
        .unwrap()
    }

    #[derive(Debug)]
    struct CodeInput(Vec<u8>);

    impl OpenFileDescription for CodeInput {
        fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
            let count = output.len().min(self.0.len());
            output[..count].copy_from_slice(&self.0[..count]);
            Ok(count)
        }
    }

    #[test]
    fn alias_observes_copyout() {
        const WRITABLE: u64 = 4096;
        const GUARD: u64 = 8192;
        const EXECUTABLE: u64 = 12_288;
        for architecture in [hl_isa::GuestArchitecture::Aarch64, hl_isa::GuestArchitecture::X86_64] {
            let (space, shared) = space();
            let object = shared.create(1, 4096).unwrap();
            let reference = hl_memory::SharedBackingRef {
                object,
                offset: 0,
                length: 4096,
                write_shared: true,
            };
            space
                .mappings()
                .map(request(
                    WRITABLE,
                    Protection::READ.union(Protection::WRITE),
                    hl_memory::Backing::Shared(reference),
                ))
                .unwrap();
            space
                .mappings()
                .map(request(
                    GUARD,
                    Protection::NONE,
                    hl_memory::Backing::Anonymous {
                        identity: 2,
                        shared: false,
                    },
                ))
                .unwrap();
            space
                .mappings()
                .map(request(
                    EXECUTABLE,
                    Protection::READ.union(Protection::EXECUTE),
                    hl_memory::Backing::Shared(reference),
                ))
                .unwrap();
            let (one, two): (&[u8], &[u8]) = match architecture {
                hl_isa::GuestArchitecture::Aarch64 => (
                    &[0x20, 0x00, 0x80, 0x52, 0x01, 0x00, 0x00, 0xd4],
                    &[0x40, 0x00, 0x80, 0x52, 0x01, 0x00, 0x00, 0xd4],
                ),
                hl_isa::GuestArchitecture::X86_64 => (&[0xb8, 1, 0, 0, 0, 0x0f, 0x05], &[0xb8, 2, 0, 0, 0, 0x0f, 0x05]),
            };
            let guest = space.guest_memory();
            assert_eq!(
                GuestMarshaller::new(&guest, architecture).copy_to(WRITABLE, one).fault,
                None
            );
            let machine = machine(architecture, EXECUTABLE);
            let mut operand = space.arena_memory();
            assert!(matches!(
                machine.run_slice(1, 4, &mut operand),
                StepOutcome::Syscall { .. }
            ));
            assert_eq!(
                machine.handle_syscall(1, |cpu| {
                    match cpu {
                        ExecutionCpuSnapshot::Aarch64(cpu) => {
                            assert_eq!(cpu.registers[0], 1);
                            cpu.pc = EXECUTABLE;
                        }
                        ExecutionCpuSnapshot::X86_64(cpu) => {
                            assert_eq!(cpu.registers[0], 1);
                            cpu.rip = EXECUTABLE;
                        }
                    }
                    StepOutcome::Continue
                }),
                StepOutcome::Continue
            );
            let before = space.mappings().observer_epoch();
            let mut replacement = vec![0; 4096];
            replacement[..two.len()].copy_from_slice(two);
            let table = Arc::new(DescriptorTable::new(1).unwrap());
            let descriptor = table
                .commit(
                    table.reserve(0).unwrap(),
                    Arc::new(CodeInput(replacement)),
                    StatusFlags::default(),
                    DescriptorFlags::default(),
                )
                .unwrap();
            let mut filesystem = hl_runtime::RuntimeFilesystemSyscalls::new(table, guest.clone(), architecture);
            assert_eq!(
                DescriptorIoSyscalls::handle(
                    &mut filesystem,
                    SyscallOperation {
                        canonical_number: 0,
                        name: "read",
                        family: SyscallFamily::DescriptorIo,
                    },
                    [descriptor as u64, WRITABLE, 8192, 0, 0, 0],
                ),
                LinuxResult::Value(4096)
            );
            assert_eq!(space.mappings().observer_epoch(), before + 1);
            assert!(matches!(
                machine.run_slice(1, 4, &mut operand),
                StepOutcome::Syscall { .. }
            ));
            assert_eq!(
                machine.handle_syscall(1, |cpu| {
                    match cpu {
                        ExecutionCpuSnapshot::Aarch64(cpu) => assert_eq!(cpu.registers[0], 2),
                        ExecutionCpuSnapshot::X86_64(cpu) => assert_eq!(cpu.registers[0], 2),
                    }
                    StepOutcome::Continue
                }),
                StepOutcome::Continue
            );
        }
    }

    #[test]
    fn snapshot_preserves_regions() {
        let (parent, shared) = space();
        parent
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
            ))
            .unwrap();
        let object = shared.create(1, 4096).unwrap();
        shared.write_growing(object, 0, b"shared").unwrap();
        let reference = hl_memory::SharedBackingRef {
            object,
            offset: 0,
            length: 4096,
            write_shared: true,
        };
        parent
            .mappings()
            .map(request(
                8192,
                Protection::EXECUTE,
                hl_memory::Backing::Shared(reference),
            ))
            .unwrap();
        parent
            .mappings()
            .map(request(
                12_288,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Shared(reference),
            ))
            .unwrap();
        parent.arena().write(0, b"private").unwrap();
        let child = parent.fork_snapshot(AddressSpaceId { slot: 2, generation: 1 }).unwrap();
        assert!(Arc::ptr_eq(
            &parent.arena().resource_context(),
            &child.arena().resource_context()
        ));
        let exec = parent.exec_space(AddressSpaceId { slot: 3, generation: 1 }).unwrap();
        assert!(Arc::ptr_eq(
            &parent.arena().resource_context(),
            &exec.arena().resource_context()
        ));
        let before = parent.mappings().snapshot().regions;
        let after = child.mappings().snapshot().regions;
        assert_eq!(before, after);
        assert_eq!(after[1].backing(), hl_memory::Backing::Shared(reference));
        assert_eq!(after[1].protection(), Protection::EXECUTE);
        assert!(
            !child
                .mappings()
                .contains(hl_isa::AddressRange::nonempty(GuestAddress::new(4096), 4096,).unwrap())
        );
        let mut bytes = [0; 6];
        child
            .arena()
            .snapshot_read(8192, &mut bytes, Protection::EXECUTE)
            .unwrap();
        assert_eq!(&bytes, b"shared");
        let mut parent_memory = parent.arena_memory();
        let write = parent_memory.reserve_write(12_288, 1).unwrap();
        parent_memory.commit_write(write, b'X' as u64).unwrap();
        assert_eq!(child.arena_memory().read(12_288, 1).unwrap(), b'X' as u64);
        parent.arena().write(0, b"changed").unwrap();
        let mut private = [0; 7];
        child.arena().read(0, &mut private).unwrap();
        assert_eq!(&private, b"private");
    }

    #[test]
    fn generation_swap_atomic() {
        use hl_execution::{AtomicValue, ExclusiveMemory, MemoryOrder};
        let (space, shared) = space();
        space
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
            ))
            .unwrap();
        space.arena().write(0, b"old").unwrap();
        let stale = space.lease();
        let mut operand = space.arena_memory();
        let exclusive = operand.load_exclusive(0, 1, false, MemoryOrder::Relaxed).unwrap();
        assert_eq!(stale.generation(), 1);

        let arena = Arc::new(
            VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            Arc::clone(&shared),
            AddressSpaceId { slot: 1, generation: 2 },
        ));
        mappings
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 2,
                    shared: false,
                },
            ))
            .unwrap();
        arena.write(0, b"new").unwrap();

        let retired = space.replace(1, arena, mappings).unwrap();
        assert_eq!(retired.generation(), 1);
        assert_eq!(space.lease().generation(), 2);
        assert!(space.replace(1, retired.arena(), retired.mappings()).is_err());
        let mut old = [0; 3];
        stale.arena().read(0, &mut old).unwrap();
        let mut new = [0; 3];
        space.arena().read(0, &mut new).unwrap();
        assert_eq!(&old, b"old");
        assert_eq!(&new, b"new");
        assert!(
            !operand
                .store_exclusive(
                    exclusive.reservation,
                    AtomicValue {
                        low: b'X' as u64,
                        high: 0
                    },
                    MemoryOrder::Relaxed,
                )
                .unwrap()
        );
        space.arena().read(0, &mut new).unwrap();
        assert_eq!(&new, b"new");
    }

    #[test]
    fn direct_copyout_scope() {
        use hl_execution::{AtomicValue, ExclusiveMemory, MemoryOrder};

        let (space, _) = space();
        space
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 41,
                    shared: false,
                },
            ))
            .unwrap();
        space.arena().write(0, &1_u64.to_le_bytes()).unwrap();
        let mut memory = space.arena_memory();
        let disjoint = memory.load_exclusive(0, 8, false, MemoryOrder::Acquire).unwrap();
        space.arena().write(128, &2_u64.to_le_bytes()).unwrap();
        assert!(
            memory
                .store_exclusive(
                    disjoint.reservation,
                    AtomicValue { low: 3, high: 0 },
                    MemoryOrder::Release,
                )
                .unwrap()
        );

        let overlapping = memory.load_exclusive(0, 8, false, MemoryOrder::Acquire).unwrap();
        space.arena().write(8, &4_u64.to_le_bytes()).unwrap();
        assert!(
            !memory
                .store_exclusive(
                    overlapping.reservation,
                    AtomicValue { low: 5, high: 0 },
                    MemoryOrder::Release,
                )
                .unwrap()
        );
    }

    #[test]
    fn alias_copyout_invalidates() {
        use hl_execution::{AtomicValue, ExclusiveMemory, MemoryOrder};

        let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
        let object = shared.create(1, 4096).unwrap();
        let reference = hl_memory::SharedBackingRef {
            object,
            offset: 0,
            length: 4096,
            write_shared: true,
        };
        let make_space = |slot| {
            let arena = Arc::new(
                VirtualMemory::reserve(16_384)
                    .unwrap()
                    .with_shared_store(Arc::clone(&shared))
                    .with_snapshot_backings(),
            );
            let mappings = Arc::new(MappingCoordinator::with_shared_space(
                MappingHostAdapter::new(Arc::clone(&arena)),
                Arc::clone(&shared),
                AddressSpaceId { slot, generation: 1 },
            ));
            AddressSpace::new(arena, mappings)
        };
        let first = make_space(1);
        let second = make_space(2);
        first
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Shared(reference),
            ))
            .unwrap();
        second
            .mappings()
            .map(request(
                4096,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Shared(reference),
            ))
            .unwrap();
        first.arena().write(0, &1_u64.to_le_bytes()).unwrap();
        let mut memory = first.arena_memory();
        let reservation = memory.load_exclusive(0, 8, false, MemoryOrder::Acquire).unwrap();

        second.arena().write(4096, &2_u64.to_le_bytes()).unwrap();

        assert!(
            !memory
                .store_exclusive(
                    reservation.reservation,
                    AtomicValue { low: 3, high: 0 },
                    MemoryOrder::Release,
                )
                .unwrap()
        );
    }

    #[test]
    fn private_peer_isolated() {
        use hl_execution::{AtomicValue, ExclusiveMemory, MemoryOrder};

        let (first, shared) = space();
        let arena = Arc::new(
            VirtualMemory::reserve(16_384)
                .unwrap()
                .with_shared_store(Arc::clone(&shared))
                .with_snapshot_backings(),
        );
        let mappings = Arc::new(MappingCoordinator::with_shared_space(
            MappingHostAdapter::new(Arc::clone(&arena)),
            shared,
            AddressSpaceId { slot: 2, generation: 1 },
        ));
        let second = AddressSpace::new(arena, mappings);
        let backing = hl_memory::Backing::Anonymous {
            identity: 73,
            shared: false,
        };
        first
            .mappings()
            .map(request(0, Protection::READ.union(Protection::WRITE), backing))
            .unwrap();
        second
            .mappings()
            .map(request(0, Protection::READ.union(Protection::WRITE), backing))
            .unwrap();
        first.arena().write(0, &1_u64.to_le_bytes()).unwrap();
        let mut memory = first.arena_memory();
        let reservation = memory.load_exclusive(0, 8, false, MemoryOrder::Acquire).unwrap();

        second.arena().write(0, &2_u64.to_le_bytes()).unwrap();

        assert!(
            memory
                .store_exclusive(
                    reservation.reservation,
                    AtomicValue { low: 3, high: 0 },
                    MemoryOrder::Release,
                )
                .unwrap()
        );
    }

    #[test]
    fn spanning_copyout_invalidates() {
        use hl_execution::{AtomicValue, ExclusiveMemory, MemoryOrder};

        let (space, shared) = space();
        let first = shared.create(1, 4096).unwrap();
        let second = shared.create(1, 4096).unwrap();
        for (address, object) in [(0, first), (4096, second)] {
            space
                .mappings()
                .map(request(
                    address,
                    Protection::READ.union(Protection::WRITE),
                    hl_memory::Backing::Shared(hl_memory::SharedBackingRef {
                        object,
                        offset: 0,
                        length: 4096,
                        write_shared: true,
                    }),
                ))
                .unwrap();
        }
        let mut memory = space.arena_memory();
        let left = memory.load_exclusive(4032, 8, false, MemoryOrder::Acquire).unwrap();
        let right = memory.load_exclusive(4096, 8, false, MemoryOrder::Acquire).unwrap();

        space.arena().write(4064, &[7; 96]).unwrap();

        for reservation in [left.reservation, right.reservation] {
            assert!(
                !memory
                    .store_exclusive(reservation, AtomicValue { low: 9, high: 0 }, MemoryOrder::Release,)
                    .unwrap()
            );
        }
    }

    #[test]
    fn failure_preserves_parent() {
        let (parent, _) = space();
        parent
            .mappings()
            .map(request(
                0,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
            ))
            .unwrap();
        parent.arena().write(0, b"parent").unwrap();
        let before = parent.mappings().snapshot();
        assert!(matches!(
            parent.fork_bounded(AddressSpaceId { slot: 2, generation: 1 }, 4095,),
            Err(Error::Capacity)
        ));
        assert_eq!(parent.mappings().snapshot(), before);
        let mut bytes = [0; 6];
        parent.arena().read(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");
    }

    #[test]
    fn unlinked_shared_fork() {
        let (parent, shared) = space();
        let object = shared.create(1, 4096).unwrap();
        let reference = hl_memory::SharedBackingRef {
            object,
            offset: 0,
            length: 4096,
            write_shared: true,
        };
        parent
            .mappings()
            .map(request(
                4096,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Shared(reference),
            ))
            .unwrap();
        let mut parent_memory = parent.arena_memory();
        let write = parent_memory.reserve_write(4096, 4).unwrap();
        parent_memory
            .commit_write(write, u32::from_le_bytes(*b"live") as u64)
            .unwrap();
        shared.remove(object).unwrap();
        assert_eq!(shared.pin(object, false).unwrap_err(), hl_memory::SharedError::NotFound);
        assert_eq!(
            parent.arena_memory().read(4096, 4).unwrap(),
            u32::from_le_bytes(*b"live") as u64
        );

        let child = parent.fork_snapshot(AddressSpaceId { slot: 2, generation: 1 }).unwrap();
        assert_eq!(
            child.mappings().snapshot().regions[0].backing(),
            hl_memory::Backing::Shared(reference)
        );
        assert_eq!(
            child.arena_memory().read(4096, 4).unwrap(),
            u32::from_le_bytes(*b"live") as u64
        );
        let mut child_memory = child.arena_memory();
        let write = child_memory.reserve_write(4096, 4).unwrap();
        child_memory
            .commit_write(write, u32::from_le_bytes(*b"peer") as u64)
            .unwrap();
        assert_eq!(
            parent.arena_memory().read(4096, 4).unwrap(),
            u32::from_le_bytes(*b"peer") as u64
        );
    }

    #[test]
    fn anonymous_shared_fork() {
        let (parent, _) = space();
        parent
            .mappings()
            .map(request(
                4096,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::Anonymous {
                    identity: 17,
                    shared: true,
                },
            ))
            .unwrap();
        parent.arena().write(4096, b"parent").unwrap();
        let child = parent.fork_snapshot(AddressSpaceId { slot: 2, generation: 1 }).unwrap();
        let mut bytes = [0; 6];
        child.arena().read(4096, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");
        child.arena().write(4096, b"child!").unwrap();
        parent.arena().read(4096, &mut bytes).unwrap();
        assert_eq!(&bytes, b"child!");
    }

    #[test]
    fn file_shared_writes() {
        let (parent, _) = space();
        let path = std::env::temp_dir().join(format!("hl-file-shared-fork-{}", std::process::id(),));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(4096).unwrap();
        let identity = hl_memory::FileIdentity { device: 7, object: 19 };
        parent.arena().register_file(identity, &file).unwrap();
        parent
            .mappings()
            .map(request(
                4096,
                Protection::READ.union(Protection::WRITE),
                hl_memory::Backing::File { identity, shared: true },
            ))
            .unwrap();
        let child = parent.fork_snapshot(AddressSpaceId { slot: 2, generation: 1 }).unwrap();

        let mut child_memory = child.arena_memory();
        let scalar = child_memory.reserve_write(4096, 4).unwrap();
        child_memory
            .commit_write(scalar, u32::from_le_bytes(*b"four") as u64)
            .unwrap();
        let batch = child_memory.reserve_write_batch(&[(4112, 8), (4120, 8)]).unwrap();
        child_memory
            .commit_write_batch(
                batch,
                &[u64::from_le_bytes(*b"eight---"), u64::from_le_bytes(*b"more----")],
            )
            .unwrap();

        let mut scalar_bytes = [0; 4];
        parent.arena().read(4096, &mut scalar_bytes).unwrap();
        assert_eq!(&scalar_bytes, b"four");
        let mut batch_bytes = [0; 16];
        parent.arena().read(4112, &mut batch_bytes).unwrap();
        assert_eq!(&batch_bytes, b"eight---more----");
        drop(child);
        drop(parent);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fork_advice_partitions() {
        let (parent, _) = space();
        for (start, identity) in [(0, 1), (4096, 2)] {
            parent
                .mappings()
                .map(request(
                    start,
                    Protection::READ.union(Protection::WRITE),
                    hl_memory::Backing::Anonymous {
                        identity,
                        shared: false,
                    },
                ))
                .unwrap();
            parent.arena().write(start, b"value").unwrap();
        }
        parent
            .arena()
            .update_advice(
                hl_isa::AddressRange::nonempty(GuestAddress::new(0), 4096).unwrap(),
                Some(ForkAdvice::Omit),
            )
            .unwrap();
        parent
            .arena()
            .update_advice(
                hl_isa::AddressRange::nonempty(GuestAddress::new(4096), 4096).unwrap(),
                Some(ForkAdvice::Wipe),
            )
            .unwrap();
        let child = parent.fork_snapshot(AddressSpaceId { slot: 2, generation: 1 }).unwrap();
        assert!(
            !child
                .mappings()
                .contains(hl_isa::AddressRange::nonempty(GuestAddress::new(0), 4096).unwrap(),)
        );
        let mut zero = [1_u8; 5];
        child.arena().read(4096, &mut zero).unwrap();
        assert_eq!(zero, [0; 5]);
        let grandchild = child.fork_snapshot(AddressSpaceId { slot: 3, generation: 1 }).unwrap();
        grandchild.arena().read(4096, &mut zero).unwrap();
        assert_eq!(zero, [0; 5]);
    }
}
