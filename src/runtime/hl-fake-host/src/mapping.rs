use crate::{FakeHost, ResourceKind};
use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{MapRequest, MappingHost, MemoryError, Protection};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug)]
pub struct MappingAdapter {
    host: FakeHost,
    reservations: Mutex<BTreeMap<u64, &'static str>>,
}

impl MappingAdapter {
    #[must_use]
    pub fn new(host: FakeHost) -> Self {
        Self {
            host,
            reservations: Mutex::new(BTreeMap::new()),
        }
    }

    fn stage(&self, operation: &'static str) -> Result<u64, MemoryError> {
        let reservation = self
            .host
            .allocate("mapping", ResourceKind::Mapping)
            .map_err(|_| MemoryError::InvariantViolation)?;
        self.host
            .record("mapping", operation, reservation, 0, 0)
            .map_err(|_| MemoryError::InvariantViolation)?;
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(reservation, operation);
        Ok(reservation)
    }
}

impl MappingHost for MappingAdapter {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        self.stage("stage-map")
    }

    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        self.stage("stage-unmap")
    }

    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        self.stage("stage-protect")
    }

    fn commit(&self, reservations: &[u64]) -> Result<(), MemoryError> {
        for reservation in reservations {
            self.reservations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(reservation)
                .ok_or(MemoryError::InvariantViolation)?;
            self.host
                .release("mapping", ResourceKind::Mapping, *reservation)
                .map_err(|_| MemoryError::InvariantViolation)?;
        }
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        if self
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation)
            .is_some()
        {
            let _ = self.host.release("mapping", ResourceKind::Mapping, reservation);
        }
    }
}
