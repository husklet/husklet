use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{
    AtomicU32Write, AtomicWriteBatchHost, Backing, BackingChange, BackingChangeHost, ExitMappingHost, MapRequest,
    MappingHost, MemoryAccessHost, MemoryError, PreparedHostExit, Protection, Region,
};

use super::VirtualMemory;
use super::arena::Operation;
use super::virtual_sparse::{BackingLease, Prepared as SparseCandidate, SparseMappings};

#[derive(Debug)]
pub struct MappingHostAdapter {
    arena: Arc<VirtualMemory>,
    sparse: Arc<SparseMappings>,
    stages: Arc<Mutex<StageState>>,
    writes: Mutex<WriteState>,
}

#[derive(Debug)]
struct StageReservation {
    arena: u64,
    sparse: SparseCandidate,
}

#[derive(Debug, Default)]
struct StageState {
    reservations: BTreeMap<u64, StageReservation>,
}

pub struct DirectProjection {
    arena: Arc<VirtualMemory>,
    token: u64,
    address: u64,
}

pub enum Projection {
    Backing(BackingLease),
    Direct(DirectProjection),
}

impl hl_memory::HostProjection for Projection {
    fn storage_address(&self) -> u64 {
        match self {
            Self::Backing(lease) => lease.address(),
            Self::Direct(lease) => lease.address,
        }
    }

    fn shared_backing_is_coherent(&self) -> bool {
        matches!(self, Self::Backing(_))
    }
}

impl hl_memory::HostProjection for DirectProjection {
    fn storage_address(&self) -> u64 {
        self.address
    }
}

impl Drop for DirectProjection {
    fn drop(&mut self) {
        self.arena.unpin_direct(self.token);
    }
}

pub struct BackingChanges {
    mappings: Arc<hl_memory::MappingCoordinator<MappingHostAdapter>>,
}

impl BackingChanges {
    #[must_use]
    pub fn new(mappings: Arc<hl_memory::MappingCoordinator<MappingHostAdapter>>) -> Self {
        Self { mappings }
    }
}

impl hl_runtime::BackingChangePort for BackingChanges {
    fn changed(&self, change: BackingChange) -> Result<(), ()> {
        self.mappings.backing_changed(change).map(drop).map_err(|_| ())
    }
}

#[derive(Debug, Default)]
struct WriteState {
    next: u64,
    plain: BTreeMap<u64, AddressRange>,
    atomic: BTreeMap<u64, Vec<AtomicU32Write>>,
}

impl MappingHostAdapter {
    #[must_use]
    pub fn new(arena: Arc<VirtualMemory>) -> Self {
        Self {
            arena,
            sparse: Arc::new(SparseMappings::default()),
            stages: Arc::new(Mutex::new(StageState::default())),
            writes: Mutex::new(WriteState::default()),
        }
    }

    fn token(state: &mut WriteState) -> u64 {
        state.next = state.next.wrapping_add(1).max(1);
        state.next
    }

    #[must_use]
    pub fn length(&self) -> usize {
        self.arena.length()
    }

    fn rollback_reservations(&self, reservations: &[u64]) {
        for token in reservations.iter().rev() {
            self.rollback(*token);
        }
    }

    fn memory_error(error: super::virtual_memory::MemoryError) -> MemoryError {
        match error {
            super::virtual_memory::MemoryError::OutOfMemory => MemoryError::ResourceLimit,
            super::virtual_memory::MemoryError::InvalidRange
            | super::virtual_memory::MemoryError::Host
            | super::virtual_memory::MemoryError::Poisoned => MemoryError::InvariantViolation,
        }
    }

    fn check_range(&self, address: u64, length: u64) -> Result<(), MemoryError> {
        self.arena
            .host_range(address, length)
            .map(|_| ())
            .map_err(Self::memory_error)
    }
}

pub struct PreparedMappingExit {
    arena: Arc<VirtualMemory>,
    sparse: Arc<SparseMappings>,
    stages: Arc<Mutex<StageState>>,
    reservations: Vec<u64>,
    published: bool,
}

impl ExitMappingHost for MappingHostAdapter {
    type PreparedExit = PreparedMappingExit;

    fn prepare_exit(&self, regions: &[Region]) -> Result<Self::PreparedExit, MemoryError> {
        let mut reservations = Vec::with_capacity(regions.len());
        for region in regions {
            match self.stage_unmap(region.range()) {
                Ok(token) => reservations.push(token),
                Err(error) => {
                    self.rollback_reservations(&reservations);
                    return Err(error);
                }
            }
        }
        Ok(PreparedMappingExit {
            arena: Arc::clone(&self.arena),
            sparse: Arc::clone(&self.sparse),
            stages: Arc::clone(&self.stages),
            reservations,
            published: false,
        })
    }
}

impl PreparedHostExit for PreparedMappingExit {
    fn publish(&mut self) -> Result<(), MemoryError> {
        self.published = true;
        Ok(())
    }

    fn rollback(&mut self) {
        for token in self.reservations.drain(..) {
            if let Some(reservation) = self
                .stages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .reservations
                .remove(&token)
            {
                self.arena.rollback(reservation.arena);
            }
        }
        self.published = false;
    }

    fn finish(&mut self) {
        if self.published {
            let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
            let arena = self
                .reservations
                .iter()
                .filter_map(|token| stages.reservations.get(token).map(|reservation| reservation.arena))
                .collect::<Vec<_>>();
            if self.arena.commit(&arena).is_ok() {
                if let Some(token) = self.reservations.last()
                    && let Some(reservation) = stages.reservations.get(token)
                {
                    self.sparse.publish(reservation.sparse.clone());
                }
                for token in &self.reservations {
                    stages.reservations.remove(token);
                }
            } else {
                for token in self.reservations.iter().rev() {
                    if let Some(reservation) = stages.reservations.remove(token) {
                        self.arena.rollback(reservation.arena);
                    }
                }
            }
            self.reservations.clear();
            self.published = false;
        }
    }
}

impl Drop for PreparedMappingExit {
    fn drop(&mut self) {
        for token in self.reservations.drain(..) {
            if let Some(reservation) = self
                .stages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .reservations
                .remove(&token)
            {
                self.arena.rollback(reservation.arena);
            }
        }
    }
}

impl MappingHost for MappingHostAdapter {
    fn reservation_epochs(&self) -> Option<Arc<hl_memory::ReservationEpochs>> {
        Some(Arc::clone(&self.arena.reservations))
    }

    fn stage_map(&self, address: GuestAddress, request: MapRequest) -> Result<u64, MemoryError> {
        if !matches!(
            request.backing,
            Backing::Anonymous { .. } | Backing::Shared(_) | Backing::File { .. }
        ) {
            return Err(MemoryError::InvariantViolation);
        }
        self.check_range(address.get(), request.length)?;
        let capability = self.arena.prepare_canonical(request).map_err(Self::memory_error)?;
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let prior = stages.reservations.last_key_value().map(|(_, reservation)| &reservation.sparse);
        let sparse = self
            .sparse
            .prepare_map(
                prior,
                address.get(),
                request.length,
                capability.as_ref().map(|capability| (&capability.file, capability.offset)),
            )
            .map_err(Self::memory_error)?;
        let arena = self
            .arena
            .stage(Operation::Map(address.get(), request))
            .map_err(Self::memory_error)?;
        stages.reservations.insert(arena, StageReservation { arena, sparse });
        Ok(arena)
    }

    fn stage_unmap(&self, range: AddressRange) -> Result<u64, MemoryError> {
        self.check_range(range.start().get(), range.length())?;
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let prior = stages.reservations.last_key_value().map(|(_, reservation)| &reservation.sparse);
        let sparse = self
            .sparse
            .prepare_unmap(prior, range.start().get(), range.length())
            .map_err(Self::memory_error)?;
        let arena = self
            .arena
            .stage(Operation::Unmap(range.start().get(), range.length()))
            .map_err(Self::memory_error)?;
        stages.reservations.insert(arena, StageReservation { arena, sparse });
        Ok(arena)
    }

    fn stage_protect(&self, range: AddressRange, protection: Protection) -> Result<u64, MemoryError> {
        self.check_range(range.start().get(), range.length())?;
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let prior = stages.reservations.last_key_value().map(|(_, reservation)| &reservation.sparse);
        let sparse = self.sparse.prepare_same(prior);
        let arena = self
            .arena
            .stage(Operation::Protect(range.start().get(), range.length(), protection))
            .map_err(Self::memory_error)?;
        stages.reservations.insert(arena, StageReservation { arena, sparse });
        Ok(arena)
    }

    fn commit(&self, reservations: &[u64]) -> Result<(), MemoryError> {
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let arena = reservations
            .iter()
            .map(|token| {
                stages
                    .reservations
                    .get(token)
                    .map(|reservation| reservation.arena)
                    .ok_or(MemoryError::InvariantViolation)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.arena.commit(&arena).map_err(Self::memory_error)?;
        if let Some(token) = reservations.last() {
            let candidate = stages
                .reservations
                .get(token)
                .ok_or(MemoryError::InvariantViolation)?
                .sparse
                .clone();
            self.sparse.publish(candidate);
        }
        for token in reservations {
            stages.reservations.remove(token);
        }
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        if let Some(reservation) = self
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reservations
            .remove(&reservation)
        {
            self.arena.rollback(reservation.arena);
        }
    }

    fn stage_remap(
        &self,
        source: AddressRange,
        destination: GuestAddress,
        request: MapRequest,
        keep_source: bool,
    ) -> Result<u64, MemoryError> {
        self.check_range(source.start().get(), source.length())?;
        self.check_range(destination.get(), request.length)?;
        let capability = self.arena.prepare_canonical(request).map_err(Self::memory_error)?;
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let prior = stages.reservations.last_key_value().map(|(_, reservation)| &reservation.sparse);
        let sparse = self
            .sparse
            .prepare_remap(
                prior,
                source,
                destination.get(),
                request.length,
                keep_source,
                capability.as_ref().map(|capability| (&capability.file, capability.offset)),
            )
            .map_err(Self::memory_error)?;
        let arena = self
            .arena
            .stage(Operation::Remap(source, destination.get(), request, keep_source))
            .map_err(Self::memory_error)?;
        stages.reservations.insert(arena, StageReservation { arena, sparse });
        Ok(arena)
    }
}

impl BackingChangeHost for MappingHostAdapter {
    fn stage_backing_change(&self, change: BackingChange, mappings: &[Region]) -> Result<u64, MemoryError> {
        if mappings.is_empty()
            || mappings.iter().any(|region| {
                !matches!(
                    region.backing(),
                    Backing::File { identity, .. } if identity == change.identity
                )
            })
        {
            return Err(MemoryError::InvariantViolation);
        }
        let mut stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        let prior = stages.reservations.last_key_value().map(|(_, reservation)| &reservation.sparse);
        let sparse = self.sparse.prepare_same(prior);
        let arena = self.arena.stage(Operation::Backing(change)).map_err(Self::memory_error)?;
        stages.reservations.insert(arena, StageReservation { arena, sparse });
        Ok(arena)
    }
}

impl MemoryAccessHost for MappingHostAdapter {
    type Projection = Projection;

    fn project(&self, range: AddressRange) -> Result<Self::Projection, MemoryError> {
        if let Some(lease) = self.sparse.pin(range).map_err(Self::memory_error)? {
            return Ok(Projection::Backing(lease));
        }
        if self.arena.bus_fault(range.start().get(), range.length()).is_some() {
            return Err(MemoryError::NoAddressSpace);
        }
        let address = self
            .arena
            .storage_address(range.start().get(), range.length())
            .ok_or(MemoryError::NoAddressSpace)?;
        let token = self.arena.pin_direct(range).map_err(Self::memory_error)?;
        Ok(Projection::Direct(DirectProjection {
            arena: Arc::clone(&self.arena),
            token,
            address,
        }))
    }

    fn project_aperture(&self) -> Result<Option<hl_memory::HostAperture<Self::Projection>>, MemoryError> {
        // Serialize sparse publication with the direct pin. Once installed,
        // the aperture-wide pin rejects every overlapping host transition.
        let _stages = self.stages.lock().unwrap_or_else(|error| error.into_inner());
        if !self.sparse.is_empty() {
            return Ok(None);
        }
        let length = self.arena.length() as u64;
        let range = AddressRange::nonempty(GuestAddress::ZERO, length).map_err(|_| MemoryError::AddressOverflow)?;
        let address = self
            .arena
            .storage_address(0, length)
            .ok_or(MemoryError::NoAddressSpace)?;
        let token = self.arena.pin_direct(range).map_err(Self::memory_error)?;
        let projection = Projection::Direct(DirectProjection {
            arena: Arc::clone(&self.arena),
            token,
            address,
        });
        hl_memory::HostAperture::new(range, projection).map(Some)
    }

    fn validate_file(
        &self,
        identity: hl_memory::FileIdentity,
        offset: u64,
        length: u64,
        address: GuestAddress,
    ) -> Result<(), MemoryError> {
        self.arena
            .validate_file(identity, offset, length, address.get())
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn file_prefix(
        &self,
        identity: hl_memory::FileIdentity,
        offset: u64,
        length: u64,
        address: GuestAddress,
    ) -> Result<u64, MemoryError> {
        self.arena
            .file_prefix(identity, offset, length, address.get())
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn read(&self, range: AddressRange, output: &mut [u8]) -> Result<(), MemoryError> {
        self.arena
            .snapshot_read(range.start().get(), output, Protection::READ)
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn prepare_write(&self, range: AddressRange) -> Result<u64, MemoryError> {
        let mut state = self.writes.lock().unwrap_or_else(|error| error.into_inner());
        let token = Self::token(&mut state);
        state.plain.insert(token, range);
        Ok(token)
    }

    fn commit_write(&self, reservation: u64, input: &[u8]) -> Result<(), MemoryError> {
        let range = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .plain
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        if range.length() != input.len() as u64 {
            return Err(MemoryError::InvariantViolation);
        }
        self.arena
            .write_untracked(range.start().get(), input)
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn write_atomic(&self, range: AddressRange, input: &[u8]) -> Result<(), MemoryError> {
        if range.length() != input.len() as u64 {
            return Err(MemoryError::InvariantViolation);
        }
        self.arena
            .write_untracked(range.start().get(), input)
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn commit_external_write(&self, reservation: u64, length: u64) -> Result<(), MemoryError> {
        let range = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .plain
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        if length > range.length() {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }

    fn rollback_write(&self, reservation: u64) {
        self.writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .plain
            .remove(&reservation);
    }
}

impl AtomicWriteBatchHost for MappingHostAdapter {
    fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<u64, MemoryError> {
        let mut state = self.writes.lock().unwrap_or_else(|error| error.into_inner());
        let token = Self::token(&mut state);
        state.atomic.insert(token, writes.to_vec());
        Ok(token)
    }

    fn commit_u32_batch(&self, reservation: u64) -> Result<(), MemoryError> {
        let writes = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .atomic
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        self.arena
            .compare_write(&writes)
            .map_err(|_| MemoryError::InvariantViolation)
    }

    fn rollback_u32_batch(&self, reservation: u64) {
        self.writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .atomic
            .remove(&reservation);
    }
}
