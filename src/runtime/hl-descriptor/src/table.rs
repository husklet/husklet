use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::checkpoint_activity::CheckpointAdmission;
use crate::model::OpenDescription;
use crate::state::TableState;
use crate::{
    CheckpointActivity, Descriptor, DescriptorError, DescriptorFlags, ExactDuplicate, OpenFileDescription,
    OperationLease, StatusFlags,
};

/// Lowest descriptor number available to ordinary allocation.
pub const FIRST_DESCRIPTOR: i32 = 0;

/// A process descriptor table.
///
/// Every mutation takes one write lock, making lookup/replacement decisions
/// transactional. Dropped entries release their description only after no
/// descriptor in this table retains its [`Arc`].
#[derive(Debug)]
pub struct DescriptorTable {
    pub(crate) state: RwLock<TableState>,
    pub(crate) next_description_identity: Arc<AtomicU64>,
    pub(crate) checkpoint: Arc<CheckpointActivity>,
    pub(crate) admission_limit: AtomicI32,
}

/// Generation-checked claim on a descriptor number.
///
/// Dropping an uncommitted reservation makes the number available again.
#[derive(Debug)]
pub struct Reservation<'table> {
    pub(crate) table: &'table DescriptorTable,
    pub(crate) number: i32,
    pub(crate) generation: u32,
    pub(crate) committed: bool,
    pub(crate) admission: Option<CheckpointAdmission>,
}

impl Reservation<'_> {
    #[must_use]
    pub const fn number(&self) -> i32 {
        self.number
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.table.cancel_reservation(self.number, self.generation);
        }
    }
}

impl DescriptorTable {
    /// Creates an empty table accepting descriptor numbers in `0..limit`.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError::InvalidArgument`] when `limit` is negative.
    pub fn new(limit: i32) -> Result<Self, DescriptorError> {
        if limit < 0 {
            return Err(DescriptorError::InvalidArgument);
        }
        let next_description_identity = Arc::new(AtomicU64::new(1));
        Ok(Self {
            state: RwLock::new(TableState {
                entries: BTreeMap::new(),
                reservations: BTreeMap::new(),
                generations: BTreeMap::new(),
                transfers: BTreeMap::new(),
                checkpoint_roots: BTreeMap::new(),
                limit,
            }),
            next_description_identity,
            checkpoint: Arc::new(CheckpointActivity::default()),
            admission_limit: AtomicI32::new(limit),
        })
    }

    /// Updates the process-visible allocation ceiling while retaining the
    /// table's construction-time safety capacity.
    pub fn set_admission_limit(&self, limit: u64) {
        let capacity = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .limit;
        self.admission_limit
            .store(limit.min(capacity as u64) as i32, Ordering::Release);
    }

    /// Reserves the lowest free descriptor at or above `minimum`.
    pub fn reserve(&self, minimum: i32) -> Result<Reservation<'_>, DescriptorError> {
        let admission = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let number = state.lowest_free_below(minimum, self.admission_limit.load(Ordering::Acquire))?;
        let generation = state.advance_generation(number);
        state.reservations.insert(number, generation);
        Ok(Reservation {
            table: self,
            number,
            generation,
            committed: false,
            admission: Some(admission),
        })
    }

    /// Reserves one exact descriptor number without replacing a live entry.
    pub fn reserve_exact(&self, number: i32) -> Result<Reservation<'_>, DescriptorError> {
        let admission = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.validate_number(number)?;
        if number >= self.admission_limit.load(Ordering::Acquire) {
            return Err(DescriptorError::BadDescriptor);
        }
        if state.entries.contains_key(&number) || state.reservations.contains_key(&number) {
            return Err(DescriptorError::AlreadyExists);
        }
        let generation = state.advance_generation(number);
        state.reservations.insert(number, generation);
        Ok(Reservation {
            table: self,
            number,
            generation,
            committed: false,
            admission: Some(admission),
        })
    }

    /// Publishes a new open file description through a reservation.
    pub fn commit(
        &self,
        mut reservation: Reservation<'_>,
        object: Arc<dyn OpenFileDescription>,
        status: StatusFlags,
        flags: DescriptorFlags,
    ) -> Result<i32, DescriptorError> {
        if !std::ptr::eq(self, reservation.table) {
            return Err(DescriptorError::InvalidArgument);
        }
        let identity = self.next_description_identity.fetch_add(1, Ordering::Relaxed);
        let description = Arc::new(OpenDescription::new(object, identity, status));
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reservations.get(&reservation.number) != Some(&reservation.generation) {
            return Err(DescriptorError::StaleReservation);
        }
        state.reservations.remove(&reservation.number);
        state.entries.insert(
            reservation.number,
            Descriptor::new(description, flags, reservation.generation),
        );
        reservation.committed = true;
        Ok(reservation.number)
    }

    /// Installs a description at the lowest free number at or above `minimum`.
    ///
    /// This implements the allocation rule shared by open, `dup`, and
    /// `F_DUPFD`.
    pub fn install(
        &self,
        minimum: i32,
        description: Arc<dyn OpenFileDescription>,
        flags: DescriptorFlags,
    ) -> Result<i32, DescriptorError> {
        let reservation = self.reserve(minimum)?;
        self.commit(reservation, description, StatusFlags::default(), flags)
    }

    /// Installs or replaces an exact descriptor number.
    ///
    /// The returned entry is the replaced descriptor, if any.
    #[cfg(test)]
    pub(crate) fn install_exact(
        &self,
        number: i32,
        description: Arc<dyn OpenFileDescription>,
        flags: DescriptorFlags,
    ) -> Result<Option<Descriptor>, DescriptorError> {
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.validate_number(number)?;
        if state.reservations.contains_key(&number) {
            return Err(DescriptorError::AlreadyExists);
        }
        let generation = state.advance_generation(number);
        let identity = self.next_description_identity.fetch_add(1, Ordering::Relaxed);
        let opened = Arc::new(OpenDescription::new(description, identity, StatusFlags::default()));
        let replaced = state.entries.insert(number, Descriptor::new(opened, flags, generation));
        drop(state);
        if let Some(descriptor) = &replaced {
            descriptor.description.release_descriptor();
        }
        Ok(replaced)
    }

    /// Returns a snapshot of one descriptor entry.
    pub(crate) fn lookup(&self, number: i32) -> Result<Descriptor, DescriptorError> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .get(&number)
            .cloned()
            .ok_or(DescriptorError::BadDescriptor)
    }

    /// Pins an admitted operation independently of descriptor lifetime.
    pub fn pin(&self, number: i32) -> Result<OperationLease, DescriptorError> {
        self.checkpoint.admit()?;
        self.pin_entry(number, true)
    }

    pub(crate) fn pin_restored(&self, number: i32) -> Result<OperationLease, DescriptorError> {
        self.pin_entry(number, false)
    }

    fn pin_entry(&self, number: i32, admitted: bool) -> Result<OperationLease, DescriptorError> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptor = if let Some(descriptor) = state.entries.get(&number) {
            descriptor
        } else {
            if admitted {
                self.checkpoint.release();
            }
            return Err(DescriptorError::BadDescriptor);
        };
        let description = descriptor.description.clone();
        let descriptor_generation = descriptor.generation;
        description.active_operations.fetch_add(1, Ordering::AcqRel);
        Ok(OperationLease {
            description,
            descriptor_number: number,
            descriptor_generation,
            checkpoint: self.checkpoint.clone(),
            admitted,
        })
    }

    /// Updates only the descriptor-local flags.
    pub fn set_flags(&self, number: i32, flags: DescriptorFlags) -> Result<(), DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptor = state.entries.get_mut(&number).ok_or(DescriptorError::BadDescriptor)?;
        descriptor.flags = flags;
        Ok(())
    }

    /// Reads descriptor-local flags without exposing the table entry.
    pub fn flags(&self, number: i32) -> Result<DescriptorFlags, DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .get(&number)
            .map(|descriptor| descriptor.flags)
            .ok_or(DescriptorError::BadDescriptor)
    }

    /// Closes one descriptor number.
    pub fn close(&self, number: i32) -> Result<(), DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let descriptor = state.entries.remove(&number).ok_or(DescriptorError::BadDescriptor)?;
        state.advance_generation(number);
        drop(state);
        descriptor.description.release_descriptor();
        drop(descriptor);
        Ok(())
    }

    /// Duplicates onto the lowest free descriptor at or above `minimum`.
    ///
    /// The new descriptor always starts with the supplied local flags. Plain
    /// `dup` and `F_DUPFD` pass [`DescriptorFlags::default`].
    pub fn duplicate(&self, source: i32, minimum: i32, flags: DescriptorFlags) -> Result<i32, DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = state.entries.get(&source).ok_or(DescriptorError::BadDescriptor)?;
        let description = source.description.clone();
        let transfer_dependencies = source.transfer_dependencies.clone();
        let admission_limit = self.admission_limit.load(Ordering::Acquire);
        if minimum >= admission_limit {
            return Err(DescriptorError::InvalidArgument);
        }
        let destination = state.lowest_free_below(minimum, admission_limit)?;
        let generation = state.advance_generation(destination);
        description.retain_descriptor();
        state.entries.insert(
            destination,
            Descriptor::transferred(description, flags, generation, transfer_dependencies),
        );
        Ok(destination)
    }

    /// Implements the atomic descriptor-table portion of `dup2` and `dup3`.
    pub fn duplicate_exact(
        &self,
        source: i32,
        destination: i32,
        operation: ExactDuplicate,
    ) -> Result<i32, DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_descriptor = state.entries.get(&source).ok_or(DescriptorError::BadDescriptor)?;
        let description = source_descriptor.description.clone();
        let transfer_dependencies = source_descriptor.transfer_dependencies.clone();

        if source == destination {
            return match operation {
                ExactDuplicate::Dup2 => Ok(destination),
                ExactDuplicate::Dup3(_) => Err(DescriptorError::InvalidArgument),
            };
        }
        state.validate_number(destination)?;
        if destination >= self.admission_limit.load(Ordering::Acquire) {
            return Err(DescriptorError::BadDescriptor);
        }

        let flags = match operation {
            ExactDuplicate::Dup2 => DescriptorFlags::default(),
            ExactDuplicate::Dup3(flags) => flags,
        };
        let generation = state.advance_generation(destination);
        description.retain_descriptor();
        let replaced = state.entries.insert(
            destination,
            Descriptor::transferred(description, flags, generation, transfer_dependencies),
        );
        drop(state);
        if let Some(descriptor) = replaced {
            descriptor.description.release_descriptor();
        }
        Ok(destination)
    }

    /// Removes all close-on-exec descriptors and returns their former numbers.
    pub fn close_on_exec(&self) -> Vec<i32> {
        let _checkpoint = self.checkpoint.operation_wait();
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let closed: Vec<i32> = state
            .entries
            .iter()
            .filter_map(|(number, descriptor)| descriptor.flags.closes_on_exec().then_some(*number))
            .collect();
        let mut removed = Vec::with_capacity(closed.len());
        for number in &closed {
            if let Some(descriptor) = state.entries.remove(number) {
                state.advance_generation(*number);
                removed.push(descriptor);
            }
        }
        drop(state);
        for descriptor in removed {
            descriptor.description.release_descriptor();
        }
        closed
    }

    /// Clones the table for `fork`, preserving descriptor numbers, local flags,
    /// and shared open-file descriptions.
    #[must_use]
    pub fn fork(&self) -> Self {
        let _checkpoint = self.checkpoint.operation_wait();
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = state.entries.clone();
        for descriptor in entries.values() {
            descriptor.description.retain_descriptor();
        }
        Self {
            state: RwLock::new(TableState {
                entries,
                reservations: BTreeMap::new(),
                generations: state.generations.clone(),
                transfers: BTreeMap::new(),
                checkpoint_roots: BTreeMap::new(),
                limit: state.limit,
            }),
            next_description_identity: self.next_description_identity.clone(),
            checkpoint: Arc::new(CheckpointActivity::default()),
            admission_limit: AtomicI32::new(self.admission_limit.load(Ordering::Acquire)),
        }
    }

    /// Verifies reference counts, generation publication, and reservation
    /// exclusion. This is intended for debug gates and differential tests.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .reservations
            .keys()
            .any(|number| state.entries.contains_key(number))
        {
            return Err(DescriptorError::Corrupt);
        }
        let mut references = BTreeMap::<u64, u32>::new();
        for (number, descriptor) in &state.entries {
            if state.generations.get(number) != Some(&descriptor.generation) {
                return Err(DescriptorError::Corrupt);
            }
            *references.entry(descriptor.description.identity).or_default() += 1;
        }
        for descriptor in state.entries.values() {
            let local = references
                .get(&descriptor.description.identity)
                .copied()
                .unwrap_or_default();
            let global = descriptor.description.descriptor_references.load(Ordering::Acquire);
            if global < local || global == 0 {
                return Err(DescriptorError::Corrupt);
            }
        }
        Ok(())
    }

    /// Returns the number of open descriptors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }

    /// Reports whether no descriptors are open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn cancel_reservation(&self, number: i32, generation: u32) {
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reservations.get(&number) == Some(&generation) {
            state.reservations.remove(&number);
        }
    }

    /// Prevents new operations and waits for admitted operations to finish.
    pub fn freeze_checkpoint(&self) {
        self.checkpoint.freeze();
        drop(self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    /// Reopens a table after checkpoint capture or restore.
    pub fn thaw_checkpoint(&self) {
        self.checkpoint.thaw();
    }
}
