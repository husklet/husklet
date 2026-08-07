//! Guest memory access and atomic write batching for the mapping host adapter.

use std::sync::Arc;

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{AtomicU32Write, AtomicWriteBatchHost, MemoryAccessHost, MemoryError, Protection};

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

    fn prepare_write(&self, range: AddressRange) -> Result<u64, MemoryError> {
        let mut state = self.writes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = Self::token(&mut state);
        state.plain.insert(token, range);
        Ok(token)
    }

    fn commit_write(&self, reservation: u64, input: &[u8]) -> Result<(), MemoryError> {
        let range = self
            .writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .plain
            .remove(&reservation);
    }
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
