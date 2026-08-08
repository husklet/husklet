//! Guest memory access and atomic write batching for the mapping host adapter.

use std::sync::Arc;

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{AtomicU32Write, AtomicWriteBatchHost, MemoryAccessHost, MemoryError, Protection, WriteReservation};

use super::mapping::{DirectProjection, MappingHostAdapter, Projection};

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
        let _stages = self.stages.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

    fn read(&self, range: AddressRange, output: &mut [u8], access: Protection) -> Result<(), MemoryError> {
        self.arena
            .snapshot_read(range.start().get(), output, access)
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn prepare_write(&self, range: AddressRange) -> Result<WriteReservation, MemoryError> {
        // The arena addresses the range directly, so the reservation carries it
        // by value instead of parking it in a mutex-guarded side table.
        Ok(WriteReservation::new(0, range))
    }

    fn commit_write(&self, reservation: WriteReservation, input: &[u8]) -> Result<(), MemoryError> {
        let range = reservation.range;
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

    fn compare_exchange_atomic(
        &self,
        range: AddressRange,
        expected: u64,
        replacement: u64,
    ) -> Result<Option<u64>, MemoryError> {
        // A sparse lease or a bus-fault hole is not directly addressable, so
        // the coordinator keeps its serialized fallback for those.
        if self.sparse.pin(range).map_err(Self::memory_error)?.is_some()
            || self.arena.bus_fault(range.start().get(), range.length()).is_some()
        {
            return Ok(None);
        }
        self.arena
            .compare_exchange_untracked(range.start().get(), range.length(), expected, replacement)
            .map(Some)
            .map_err(|_| MemoryError::NoAddressSpace)
    }

    fn commit_external_write(&self, reservation: WriteReservation, length: u64) -> Result<(), MemoryError> {
        if length > reservation.range.length() {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }

    fn rollback_write(&self, _reservation: WriteReservation) {}
}

impl AtomicWriteBatchHost for MappingHostAdapter {
    fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<u64, MemoryError> {
        let mut state = self.writes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = Self::token(&mut state);
        state.atomic.insert(token, writes.to_vec());
        Ok(token)
    }

    fn commit_u32_batch(&self, reservation: u64) -> Result<(), MemoryError> {
        let writes = self
            .writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .atomic
            .remove(&reservation);
    }
}
