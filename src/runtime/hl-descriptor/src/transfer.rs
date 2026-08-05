use std::sync::Arc;

use crate::checkpoint_activity::CheckpointAdmission;
use crate::model::OpenDescription;
use crate::{Descriptor, DescriptorError, DescriptorFlags, DescriptorTable, OpenFileDescription, StatusFlags};

/// Non-forgeable owned transfer reference to an open file description.
#[derive(Debug)]
pub struct DescriptionRef {
    pub(crate) description: Arc<OpenDescription>,
    dependencies: Vec<DescriptionRef>,
}

impl DescriptionRef {
    pub(crate) fn new(description: Arc<OpenDescription>) -> Self {
        let dependencies = description.object.transfer_dependencies();
        description.retain_transfer();
        Self {
            description,
            dependencies,
        }
    }

    pub(crate) fn shallow(description: Arc<OpenDescription>) -> Self {
        description.retain_transfer();
        Self {
            description,
            dependencies: Vec::new(),
        }
    }

    #[must_use]
    pub fn identity(&self) -> u64 {
        self.description.identity
    }

    #[must_use]
    pub fn description_identity(&self) -> crate::DescriptionIdentity {
        crate::DescriptionIdentity {
            identity: self.description.identity,
            generation: self.description.generation,
        }
    }
}

impl Clone for DescriptionRef {
    fn clone(&self) -> Self {
        self.description.retain_transfer();
        Self {
            description: self.description.clone(),
            dependencies: self.dependencies.clone(),
        }
    }
}

impl Drop for DescriptionRef {
    fn drop(&mut self) {
        self.description.release_transfer();
    }
}

pub struct DescriptionInstallTransaction<'table, 'rights> {
    table: &'table DescriptorTable,
    rights: &'rights [DescriptionRef],
    numbers: Vec<(i32, u32)>,
    flags: DescriptorFlags,
    committed: bool,
}

impl DescriptionInstallTransaction<'_, '_> {
    #[must_use]
    pub fn numbers(&self) -> Vec<i32> {
        self.numbers.iter().map(|(number, _)| *number).collect()
    }

    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn commit(mut self) -> Result<Vec<i32>, DescriptorError> {
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (number, generation) in &self.numbers {
            if state.reservations.get(number) != Some(generation) {
                return Err(DescriptorError::StaleReservation);
            }
        }
        for ((number, generation), transferred) in self.numbers.iter().copied().zip(self.rights) {
            transferred.description.retain_descriptor();
            state.reservations.remove(&number);
            state.entries.insert(
                number,
                Descriptor::transferred(
                    transferred.description.clone(),
                    self.flags,
                    generation,
                    transferred.dependencies.clone(),
                ),
            );
        }
        self.committed = true;
        Ok(self.numbers())
    }
}

impl Drop for DescriptionInstallTransaction<'_, '_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (number, generation) in &self.numbers {
            if state.reservations.get(number) == Some(generation) {
                state.reservations.remove(number);
            }
        }
    }
}

impl DescriptorTable {
    /// Exports a non-forgeable description capability for SCM or fork transfer.
    /// A weak durable-root registration lets checkpoint discover queue-only
    /// references without extending their ordinary lifetime.
    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn export_description(&self, number: i32) -> Result<DescriptionRef, DescriptorError> {
        let descriptor = self.lookup(number)?;
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transfers
            .insert(descriptor.description.identity, Arc::downgrade(&descriptor.description));
        Ok(DescriptionRef::new(descriptor.description))
    }

    /// Installs a transferred description under a newly allocated descriptor.
    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn install_description(
        &self,
        minimum: i32,
        transferred: &DescriptionRef,
        flags: DescriptorFlags,
    ) -> Result<i32, DescriptorError> {
        let _checkpoint = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let number = state.lowest_free(minimum)?;
        let generation = state.advance_generation(number);
        transferred.description.retain_descriptor();
        state.entries.insert(
            number,
            Descriptor::transferred(
                transferred.description.clone(),
                flags,
                generation,
                transferred.dependencies.clone(),
            ),
        );
        Ok(number)
    }

    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn prepare_descriptions<'table, 'rights>(
        &'table self,
        minimum: i32,
        transferred: &'rights [DescriptionRef],
        flags: DescriptorFlags,
    ) -> Result<DescriptionInstallTransaction<'table, 'rights>, DescriptorError> {
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut selected = Vec::with_capacity(transferred.len());
        let mut candidate = minimum;
        for _ in transferred {
            candidate = state.lowest_free(candidate)?;
            selected.push(candidate);
            candidate = candidate.checked_add(1).ok_or(DescriptorError::TooManyOpenFiles)?;
        }
        let numbers = selected
            .into_iter()
            .map(|number| {
                let generation = state.advance_generation(number);
                state.reservations.insert(number, generation);
                (number, generation)
            })
            .collect();
        Ok(DescriptionInstallTransaction {
            table: self,
            rights: transferred,
            numbers,
            flags,
            committed: false,
        })
    }

    /// Atomically installs a complete `SCM_RIGHTS` transfer set.
    ///
    /// Capacity and target generations are determined before any description
    /// reference becomes visible.
    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn install_descriptions(
        &self,
        minimum: i32,
        transferred: &[DescriptionRef],
        flags: DescriptorFlags,
    ) -> Result<Vec<i32>, DescriptorError> {
        self.prepare_descriptions(minimum, transferred, flags)?.commit()
    }
}

/// Unpublished descriptor installation retained across an external transaction.
pub struct PreparedDescriptorInstall<'table> {
    table: &'table DescriptorTable,
    number: i32,
    generation: u32,
    description: Arc<OpenDescription>,
    flags: DescriptorFlags,
    _admission: CheckpointAdmission,
    published: bool,
}

impl PreparedDescriptorInstall<'_> {
    #[must_use]
    pub const fn number(&self) -> i32 {
        self.number
    }

    #[must_use]
    pub fn description_identity(&self) -> crate::DescriptionIdentity {
        crate::DescriptionIdentity {
            identity: self.description.identity,
            generation: self.description.generation,
        }
    }

    /// Publishes the descriptor after the external transaction commits.
    ///
    /// The retained checkpoint admission and private reservation make this
    /// final step infallible.
    #[must_use]
    pub fn publish(mut self) -> i32 {
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert_eq!(state.reservations.get(&self.number), Some(&self.generation),);
        state.reservations.remove(&self.number);
        state.entries.insert(
            self.number,
            Descriptor::new(self.description.clone(), self.flags, self.generation),
        );
        self.published = true;
        self.number
    }
}

impl Drop for PreparedDescriptorInstall<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reservations.get(&self.number) == Some(&self.generation) {
            state.reservations.remove(&self.number);
        }
    }
}

/// Unpublished atomic installation of several newly created descriptions.
pub struct PreparedInstallBatch<'table> {
    table: &'table DescriptorTable,
    entries: Vec<(i32, u32, Arc<OpenDescription>, DescriptorFlags)>,
    _admission: CheckpointAdmission,
    published: bool,
}

impl PreparedInstallBatch<'_> {
    #[must_use]
    pub fn numbers(&self) -> Vec<i32> {
        self.entries.iter().map(|entry| entry.0).collect()
    }

    #[must_use]
    pub fn description_identities(&self) -> Vec<crate::DescriptionIdentity> {
        self.entries
            .iter()
            .map(|entry| crate::DescriptionIdentity {
                identity: entry.2.identity,
                generation: entry.2.generation,
            })
            .collect()
    }

    /// Publishes every descriptor atomically under one table write lock.
    #[must_use]
    pub fn publish_all(mut self) -> Vec<i32> {
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (number, generation, _, _) in &self.entries {
            debug_assert_eq!(state.reservations.get(number), Some(generation));
        }
        for (number, generation, description, flags) in &self.entries {
            state.reservations.remove(number);
            state
                .entries
                .insert(*number, Descriptor::new(description.clone(), *flags, *generation));
        }
        self.published = true;
        self.numbers()
    }
}

impl Drop for PreparedInstallBatch<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut state = self
            .table
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (number, generation, _, _) in &self.entries {
            if state.reservations.get(number) == Some(generation) {
                state.reservations.remove(number);
            }
        }
    }
}

impl DescriptorTable {
    /// Attaches an object to a descriptor number reserved before external open
    /// side effects begin.
    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn prepare_reserved(
        &self,
        mut reservation: crate::Reservation<'_>,
        object: Arc<dyn OpenFileDescription>,
        status: StatusFlags,
        flags: DescriptorFlags,
    ) -> Result<PreparedDescriptorInstall<'_>, DescriptorError> {
        if !std::ptr::eq(self, reservation.table) {
            return Err(DescriptorError::InvalidArgument);
        }
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reservations.get(&reservation.number) != Some(&reservation.generation) {
            return Err(DescriptorError::StaleReservation);
        }
        drop(state);
        let identity = self
            .next_description_identity
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let admission = reservation.admission.take().ok_or(DescriptorError::Corrupt)?;
        reservation.committed = true;
        Ok(PreparedDescriptorInstall {
            table: self,
            number: reservation.number,
            generation: reservation.generation,
            description: Arc::new(OpenDescription::new(object, identity, status)),
            flags,
            _admission: admission,
            published: false,
        })
    }

    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn prepare_open(
        &self,
        minimum: i32,
        object: Arc<dyn OpenFileDescription>,
        status: StatusFlags,
        flags: DescriptorFlags,
    ) -> Result<PreparedDescriptorInstall<'_>, DescriptorError> {
        let admission = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let number = state.lowest_free(minimum)?;
        let generation = state.advance_generation(number);
        state.reservations.insert(number, generation);
        let identity = self
            .next_description_identity
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(state);
        Ok(PreparedDescriptorInstall {
            table: self,
            number,
            generation,
            description: Arc::new(OpenDescription::new(object, identity, status)),
            flags,
            _admission: admission,
            published: false,
        })
    }

    ///
    /// # Errors
    /// Returns an error if a descriptor is invalid or the requested installation cannot be reserved.
    pub fn prepare_open_batch(
        &self,
        minimum: i32,
        objects: Vec<(Arc<dyn OpenFileDescription>, StatusFlags, DescriptorFlags)>,
    ) -> Result<PreparedInstallBatch<'_>, DescriptorError> {
        let admission = self.checkpoint.operation()?;
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut selected = Vec::with_capacity(objects.len());
        let mut candidate = minimum;
        for _ in &objects {
            candidate = state.lowest_free(candidate)?;
            selected.push(candidate);
            candidate = candidate.checked_add(1).ok_or(DescriptorError::TooManyOpenFiles)?;
        }
        let mut entries = Vec::with_capacity(objects.len());
        for (number, (object, status, flags)) in selected.into_iter().zip(objects) {
            let generation = state.advance_generation(number);
            state.reservations.insert(number, generation);
            let identity = self
                .next_description_identity
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            entries.push((
                number,
                generation,
                Arc::new(OpenDescription::new(object, identity, status)),
                flags,
            ));
        }
        drop(state);
        Ok(PreparedInstallBatch {
            table: self,
            entries,
            _admission: admission,
            published: false,
        })
    }
}
