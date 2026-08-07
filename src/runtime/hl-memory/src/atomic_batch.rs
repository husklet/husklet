use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hl_isa::{AddressRange, GuestAddress};

use crate::{Backing, MappingCoordinator, MemoryAccessHost, MemoryError, Protection, SharedBackingPin};

pub const ATOMIC_U32_WRITE_BATCH_MAXIMUM: usize = 2_048;

/// One conditional 32-bit replacement in a host-atomic batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicU32Write {
    pub address: GuestAddress,
    pub expected: u32,
    pub replacement: u32,
}

/// Host capability required for an all-or-nothing conditional write batch.
///
/// `commit_u32_batch` must compare every current word with `expected`
/// and publish every replacement as one indivisible operation. On any error it
/// must publish no replacement. Implementations that can only issue sequential
/// writes must not implement this trait.
pub trait AtomicBatchHost: MemoryAccessHost {
    fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<u64, MemoryError>;
    fn commit_u32_batch(&self, reservation: u64) -> Result<(), MemoryError>;
    fn rollback_u32_batch(&self, reservation: u64);
}

/// A staged batch tied to the mapping generation under which it was checked.
pub struct PreparedAtomicBatch<'a, H: AtomicBatchHost> {
    coordinator: &'a MappingCoordinator<H>,
    reservation: u64,
    generation: u64,
    writes: Vec<AtomicU32Write>,
    committed: bool,
}

pub struct SharedAtomicBatch<H: AtomicBatchHost> {
    coordinator: Arc<MappingCoordinator<H>>,
    reservation: SharedReservation,
    generation: u64,
    writes: Vec<AtomicU32Write>,
    committed: bool,
}

enum SharedReservation {
    Host(u64),
    Object {
        pin: SharedBackingPin,
        writes: Vec<(usize, u32, u32)>,
    },
}

impl<H: AtomicBatchHost> Drop for SharedAtomicBatch<H> {
    fn drop(&mut self) {
        if !self.committed
            && let SharedReservation::Host(reservation) = &self.reservation
        {
            self.coordinator.host.rollback_u32_batch(*reservation);
        }
    }
}

impl<H: AtomicBatchHost> Drop for PreparedAtomicBatch<'_, H> {
    fn drop(&mut self) {
        if !self.committed {
            self.coordinator.host.rollback_u32_batch(self.reservation);
        }
    }
}

impl<H: AtomicBatchHost> MappingCoordinator<H> {
    fn same_object(selected: Option<&SharedBackingPin>, object: crate::SharedObjectId) -> bool {
        selected.is_none_or(|current| current.id() == object)
    }

    pub fn prepare_shared_batch(
        self: &Arc<Self>,
        writes: &[AtomicU32Write],
    ) -> Result<SharedAtomicBatch<H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.validate_u32_batch(writes)?;
        let generation = self.ledger.generation();
        let reservation = match self.shared_writes(writes)? {
            Some((pin, writes)) => SharedReservation::Object { pin, writes },
            None => SharedReservation::Host(self.host.prepare_u32_batch(writes)?),
        };
        Ok(SharedAtomicBatch {
            coordinator: Arc::clone(self),
            reservation,
            generation,
            writes: writes.to_vec(),
            committed: false,
        })
    }

    pub fn commit_shared_batch(self: &Arc<Self>, mut prepared: SharedAtomicBatch<H>) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        if !Arc::ptr_eq(self, &prepared.coordinator) {
            return Err(MemoryError::InvariantViolation);
        }
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.ledger.generation() != prepared.generation {
            return Err(MemoryError::InvariantViolation);
        }
        match &prepared.reservation {
            SharedReservation::Host(reservation) => self.host.commit_u32_batch(*reservation)?,
            SharedReservation::Object { pin, writes } => {
                pin.compare_write_u32(writes).map_err(MemoryError::from)?;
            }
        }
        prepared.committed = true;
        for write in &prepared.writes {
            let range = AddressRange::nonempty(write.address, 4).map_err(|_| MemoryError::AddressOverflow)?;
            self.invalidate_exclusive(range)?;
        }
        let executable = prepared.writes.iter().flat_map(|write| {
            self.ledger
                .resolve(write.address, Protection::WRITE)
                .into_iter()
                .flat_map(|resolution| self.executable_write_ranges(write.address, resolution, 4))
        });
        self.host.executable.publish(executable);
        Ok(self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    pub fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<PreparedAtomicBatch<'_, H>, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        self.validate_u32_batch(writes)?;
        let generation = self.ledger.generation();
        let reservation = self.host.prepare_u32_batch(writes)?;
        Ok(PreparedAtomicBatch {
            coordinator: self,
            reservation,
            generation,
            writes: writes.to_vec(),
            committed: false,
        })
    }

    fn validate_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<(), MemoryError> {
        if writes.is_empty() {
            return Err(MemoryError::EmptyRange);
        }
        if writes.len() > ATOMIC_U32_WRITE_BATCH_MAXIMUM {
            return Err(MemoryError::InvariantViolation);
        }
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut addresses = BTreeSet::new();
        for write in writes {
            if write.address.get() % 4 != 0 || !addresses.insert(write.address) {
                return Err(MemoryError::Unaligned);
            }
            let range = AddressRange::nonempty(write.address, 4).map_err(|_| MemoryError::AddressOverflow)?;
            let resolution = self
                .ledger
                .resolve(write.address, Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            if resolution.contiguous < range.length() {
                return Err(MemoryError::NoAddressSpace);
            }
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn shared_writes(
        &self,
        writes: &[AtomicU32Write],
    ) -> Result<Option<(SharedBackingPin, Vec<(usize, u32, u32)>)>, MemoryError> {
        let mut selected: Option<SharedBackingPin> = None;
        let mut resolved = Vec::with_capacity(writes.len());
        for write in writes {
            let resolution = self
                .ledger
                .resolve(write.address, Protection::WRITE)
                .ok_or(MemoryError::NoAddressSpace)?;
            let Backing::Shared(reference) = resolution.region.backing() else {
                if selected.is_some() {
                    return Err(MemoryError::InvariantViolation);
                }
                return Ok(None);
            };
            let pin = self.retained_pin(resolution.region)?;
            if !Self::same_object(selected.as_ref(), reference.object) {
                return Err(MemoryError::InvariantViolation);
            }
            let offset = usize::try_from(resolution.backing_offset).map_err(|_| MemoryError::BackingOverflow)?;
            selected.get_or_insert(pin);
            resolved.push((offset, write.expected, write.replacement));
        }
        Ok(selected.map(|pin| (pin, resolved)))
    }

    pub fn commit_u32_batch(&self, mut prepared: PreparedAtomicBatch<'_, H>) -> Result<u64, MemoryError> {
        let _admission = self.activity.admit_memory()?;
        if !std::ptr::eq(self, prepared.coordinator) {
            return Err(MemoryError::InvariantViolation);
        }
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.ledger.generation() != prepared.generation {
            return Err(MemoryError::InvariantViolation);
        }
        self.host.commit_u32_batch(prepared.reservation)?;
        prepared.committed = true;
        for write in &prepared.writes {
            let range = AddressRange::nonempty(write.address, 4).map_err(|_| MemoryError::AddressOverflow)?;
            self.invalidate_exclusive(range)?;
        }
        let executable = prepared.writes.iter().flat_map(|write| {
            self.ledger
                .resolve(write.address, Protection::WRITE)
                .into_iter()
                .flat_map(|resolution| self.executable_write_ranges(write.address, resolution, 4))
        });
        self.host.executable.publish(executable);
        Ok(self.host.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }
}
