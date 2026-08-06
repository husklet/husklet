use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, RwLock};

use crate::model::OpenDescription;
use crate::state::TableState;
use crate::{
    CheckpointActivity, Descriptor, DescriptorFlags, DescriptorTable, ObjectKind, OpenFileDescription, StatusFlags,
};

pub const DESCRIPTOR_CHECKPOINT_VERSION: u32 = 1;
pub const DESCRIPTOR_CHECKPOINT_MAXIMUM: usize = 1 << 20;
pub const DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptorTableImage {
    pub version: u32,
    pub limit: i32,
    pub generations: Vec<DescriptorGenerationImage>,
    pub descriptions: Vec<OpenDescriptionImage>,
    pub entries: Vec<DescriptorEntryImage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DescriptorGenerationImage {
    pub number: i32,
    pub generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenDescriptionImage {
    pub identity: u64,
    pub generation: u32,
    pub offset: u64,
    pub status: StatusFlags,
    pub kind: ObjectKind,
    pub object: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DescriptorEntryImage {
    pub number: i32,
    pub generation: u32,
    pub flags: DescriptorFlags,
    pub description_identity: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorCheckpointError {
    Version,
    Limit,
    DuplicateNumber,
    DuplicateDescription,
    MissingDescription,
    StaleGeneration,
    InvalidDescription,
    Object,
}

pub trait DescriptorObjectCheckpoint: Send + Sync {
    /// Returns the exact object-image size for a generation-qualified description.
    ///
    /// # Errors
    /// Returns an error if the description is invalid, exceeds checkpoint limits, or
    /// the object cannot report a stable checkpoint size.
    fn snapshot_size_identity(
        &self,
        identity: crate::DescriptionIdentity,
        object: &dyn OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError> {
        self.snapshot_size(identity.identity, object)
    }

    /// Returns the exact number of bytes required by [`Self::snapshot_into`].
    ///
    /// # Errors
    /// Returns an error if the description is invalid, exceeds checkpoint limits, or
    /// the object cannot report a stable checkpoint size.
    fn snapshot_size(
        &self,
        identity: u64,
        object: &dyn OpenFileDescription,
    ) -> Result<usize, DescriptorCheckpointError>;

    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    fn snapshot_into(
        &self,
        identity: u64,
        object: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError>;

    /// Writes a generation-qualified object image into an exactly sized buffer.
    ///
    /// # Errors
    /// Returns an error if the description is invalid, the buffer has the wrong size,
    /// or object checkpointing fails.
    fn snapshot_into_identity(
        &self,
        identity: crate::DescriptionIdentity,
        object: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        self.snapshot_into(identity.identity, object, output)
    }

    /// Allocates an exactly sized standalone object image.
    ///
    /// # Errors
    /// Returns an error if sizing or serialization fails, or if the object image exceeds
    /// the descriptor checkpoint byte limit.
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        let size = self.snapshot_size(identity, object)?;
        if size > DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
            return Err(DescriptorCheckpointError::Limit);
        }
        let mut output = vec![0; size];
        self.snapshot_into(identity, object, &mut output)?;
        Ok(output)
    }

    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError>;
}

impl DescriptorTable {
    /// Pins an OFD while a replacement table is checkpoint-frozen.
    ///
    /// The pin retains ownership but is not an active guest operation, so a
    /// durable epoll watch does not prevent later checkpoint quiescence.
    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    pub fn pin_checkpoint(&self, number: i32) -> Result<crate::OperationLease, crate::DescriptorError> {
        if !self.checkpoint.frozen() {
            return Err(crate::DescriptorError::Corrupt);
        }
        self.pin_restored(number)
    }

    /// Exports an OFD by durable identity while a replacement table is frozen.
    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    pub fn export_checkpoint_identity(&self, identity: u64) -> Result<crate::DescriptionRef, crate::DescriptorError> {
        if !self.checkpoint.frozen() {
            return Err(crate::DescriptorError::Corrupt);
        }
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let description = state
            .entries
            .values()
            .find(|entry| entry.description.identity == identity)
            .map(|entry| entry.description.clone())
            .or_else(|| state.checkpoint_roots.get(&identity).cloned())
            .ok_or(crate::DescriptorError::BadDescriptor)?;
        Ok(crate::DescriptionRef::new(description))
    }

    /// Drops temporary strong roots after dependent checkpoint participants
    /// have rebound their queued transfer references.
    pub fn release_checkpoint_roots(&self) {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .checkpoint_roots
            .clear();
    }

    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    pub fn checkpoint_image(
        &self,
        objects: &dyn DescriptorObjectCheckpoint,
    ) -> Result<DescriptorTableImage, DescriptorCheckpointError> {
        if !self.checkpoint.frozen() {
            return Err(DescriptorCheckpointError::StaleGeneration);
        }
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.reservations.is_empty() {
            return Err(DescriptorCheckpointError::StaleGeneration);
        }
        let (mut checkpoint_bytes, (generations, mut entries)) =
            preflight_structure(state.generations.len(), state.entries.len(), || {
                let generations: Vec<DescriptorGenerationImage> = state
                    .generations
                    .iter()
                    .map(|(number, generation)| DescriptorGenerationImage {
                        number: *number,
                        generation: *generation,
                    })
                    .collect();
                let entries = Vec::with_capacity(state.entries.len());
                (generations, entries)
            })?;
        let mut descriptions = BTreeMap::new();
        for (number, descriptor) in &state.entries {
            let description = &descriptor.description;
            entries.push(DescriptorEntryImage {
                number: *number,
                generation: descriptor.generation,
                flags: descriptor.flags,
                description_identity: description.identity,
            });
            if descriptions.contains_key(&description.identity) {
                continue;
            }
            let description_state = description
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let object = snapshot_object(
                objects,
                crate::DescriptionIdentity {
                    identity: description.identity,
                    generation: description.generation,
                },
                description.object.as_ref(),
                &mut checkpoint_bytes,
            )?;
            descriptions.insert(
                description.identity,
                OpenDescriptionImage {
                    identity: description.identity,
                    generation: description.generation,
                    offset: description_state.offset,
                    status: description_state.status,
                    kind: description.object.kind(),
                    object,
                },
            );
        }
        for description in state.transfers.values().filter_map(std::sync::Weak::upgrade) {
            if descriptions.contains_key(&description.identity) {
                continue;
            }
            let description_state = description
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let object = snapshot_object(
                objects,
                crate::DescriptionIdentity {
                    identity: description.identity,
                    generation: description.generation,
                },
                description.object.as_ref(),
                &mut checkpoint_bytes,
            )?;
            descriptions.insert(
                description.identity,
                OpenDescriptionImage {
                    identity: description.identity,
                    generation: description.generation,
                    offset: description_state.offset,
                    status: description_state.status,
                    kind: description.object.kind(),
                    object,
                },
            );
        }
        let image = DescriptorTableImage {
            version: DESCRIPTOR_CHECKPOINT_VERSION,
            limit: state.limit,
            generations,
            descriptions: descriptions.into_values().collect(),
            entries,
        };
        image.validate()?;
        Ok(image)
    }

    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    pub fn restore_checkpoint(
        image: &DescriptorTableImage,
        objects: &dyn DescriptorObjectCheckpoint,
    ) -> Result<Self, DescriptorCheckpointError> {
        image.validate()?;
        let mut rebound = BTreeMap::new();
        let mut checkpoint_roots = BTreeMap::new();
        let mut reference_counts = BTreeMap::<u64, u32>::new();
        for entry in &image.entries {
            *reference_counts.entry(entry.description_identity).or_default() += 1;
        }
        for description in &image.descriptions {
            let object = objects.rebind(description)?;
            if object.kind() != description.kind {
                return Err(DescriptorCheckpointError::Object);
            }
            let reference_count = reference_counts.get(&description.identity).copied().unwrap_or(0);
            let restored = Arc::new(OpenDescription::restored(
                object,
                description.identity,
                description.generation,
                description.offset,
                description.status,
                reference_count,
            ));
            if reference_count == 0 {
                checkpoint_roots.insert(description.identity, restored.clone());
            }
            rebound.insert(description.identity, restored);
        }
        let generations = image
            .generations
            .iter()
            .map(|value| (value.number, value.generation))
            .collect();
        let mut entries = BTreeMap::new();
        for entry in &image.entries {
            let description = rebound
                .get(&entry.description_identity)
                .ok_or(DescriptorCheckpointError::MissingDescription)?
                .clone();
            entries.insert(
                entry.number,
                Descriptor::new(description, entry.flags, entry.generation),
            );
        }
        let next = image
            .descriptions
            .iter()
            .try_fold(1_u64, |next, value| {
                value.identity.checked_add(1).map(|candidate| next.max(candidate))
            })
            .ok_or(DescriptorCheckpointError::InvalidDescription)?;
        Ok(Self {
            state: RwLock::new(TableState {
                entries,
                reservations: BTreeMap::new(),
                generations,
                transfers: BTreeMap::new(),
                checkpoint_roots,
                limit: image.limit,
            }),
            next_description_identity: Arc::new(AtomicU64::new(next)),
            checkpoint: Arc::new(CheckpointActivity::default()),
            admission_limit: AtomicI32::new(image.limit),
        })
    }
}

fn preflight_structure<T>(
    generations: usize,
    entries: usize,
    materialize: impl FnOnce() -> T,
) -> Result<(usize, T), DescriptorCheckpointError> {
    let bytes = 20_usize
        .checked_add(generations.checked_mul(8).ok_or(DescriptorCheckpointError::Limit)?)
        .and_then(|size| size.checked_add(entries.checked_mul(20)?))
        .ok_or(DescriptorCheckpointError::Limit)?;
    if bytes > DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
        return Err(DescriptorCheckpointError::Limit);
    }
    Ok((bytes, materialize()))
}

fn snapshot_object(
    objects: &dyn DescriptorObjectCheckpoint,
    identity: crate::DescriptionIdentity,
    object: &dyn OpenFileDescription,
    checkpoint_bytes: &mut usize,
) -> Result<Vec<u8>, DescriptorCheckpointError> {
    let object_bytes = objects.snapshot_size_identity(identity, object)?;
    let next = checkpoint_bytes
        .checked_add(32)
        .and_then(|size| size.checked_add(object_bytes))
        .ok_or(DescriptorCheckpointError::Limit)?;
    if next > DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
        return Err(DescriptorCheckpointError::Limit);
    }
    let mut output = vec![0; object_bytes];
    objects.snapshot_into_identity(identity, object, &mut output)?;
    *checkpoint_bytes = next;
    Ok(output)
}

impl DescriptorTableImage {
    ///
    /// # Errors
    /// Returns an error if descriptor state is invalid or object checkpointing fails.
    pub fn validate(&self) -> Result<(), DescriptorCheckpointError> {
        if self.version != DESCRIPTOR_CHECKPOINT_VERSION {
            return Err(DescriptorCheckpointError::Version);
        }
        let limit = usize::try_from(self.limit).map_err(|_| DescriptorCheckpointError::Limit)?;
        if limit > DESCRIPTOR_CHECKPOINT_MAXIMUM
            || self.entries.len() > limit
            || self.generations.len() > limit
            || self.descriptions.len() > limit
        {
            return Err(DescriptorCheckpointError::Limit);
        }
        let object_bytes = self
            .descriptions
            .iter()
            .try_fold(0_usize, |size, value| size.checked_add(value.object.len()))
            .ok_or(DescriptorCheckpointError::Limit)?;
        let checkpoint_bytes = 20_usize
            .checked_add(
                self.generations
                    .len()
                    .checked_mul(8)
                    .ok_or(DescriptorCheckpointError::Limit)?,
            )
            .and_then(|size| size.checked_add(self.entries.len().checked_mul(20)?))
            .and_then(|size| size.checked_add(self.descriptions.len().checked_mul(32)?))
            .and_then(|size| size.checked_add(object_bytes))
            .ok_or(DescriptorCheckpointError::Limit)?;
        if checkpoint_bytes > DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM {
            return Err(DescriptorCheckpointError::Limit);
        }
        let mut generations = BTreeMap::new();
        for value in &self.generations {
            if value.number < 0
                || value.number >= self.limit
                || value.generation == 0
                || generations.insert(value.number, value.generation).is_some()
            {
                return Err(DescriptorCheckpointError::DuplicateNumber);
            }
        }
        let mut descriptions = BTreeSet::new();
        for value in &self.descriptions {
            if value.identity == 0 || value.generation == 0 || !descriptions.insert(value.identity) {
                return Err(DescriptorCheckpointError::DuplicateDescription);
            }
        }
        let mut numbers = BTreeSet::new();
        let mut referenced = BTreeSet::new();
        for entry in &self.entries {
            if entry.number < 0 || entry.number >= self.limit || !numbers.insert(entry.number) {
                return Err(DescriptorCheckpointError::DuplicateNumber);
            }
            if generations.get(&entry.number) != Some(&entry.generation) {
                return Err(DescriptorCheckpointError::StaleGeneration);
            }
            if !descriptions.contains(&entry.description_identity) {
                return Err(DescriptorCheckpointError::MissingDescription);
            }
            referenced.insert(entry.description_identity);
        }
        if !referenced.is_subset(&descriptions) {
            return Err(DescriptorCheckpointError::MissingDescription);
        }
        Ok(())
    }
}

#[cfg(test)]
mod structural_preflight_test {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn rejects_before_materialization() {
        let materializations = AtomicUsize::new(0);
        let codec_writes = AtomicUsize::new(0);
        let maximum_generations = (DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM - 20) / 8;
        for generations in [maximum_generations + 1, usize::MAX] {
            assert_eq!(
                preflight_structure(generations, 0, || {
                    materializations.fetch_add(1, Ordering::SeqCst);
                    codec_writes.fetch_add(1, Ordering::SeqCst);
                }),
                Err(DescriptorCheckpointError::Limit)
            );
        }
        assert_eq!(materializations.load(Ordering::SeqCst), 0);
        assert_eq!(codec_writes.load(Ordering::SeqCst), 0);
    }
}
