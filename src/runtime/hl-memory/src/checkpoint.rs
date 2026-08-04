use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Backing, MemoryError, MemoryLedger, MemoryLedgerSnapshot, Region, SharedError, SharedLimits, SharedObjectId,
    SharedObjectStore, SharedStoreSnapshot,
};

pub const MEMORY_CHECKPOINT_VERSION: u32 = 1;
pub const MEMORY_CHECKPOINT_REGION_MAXIMUM: usize = 1 << 20;
pub const MEMORY_CHECKPOINT_BYTES_MAXIMUM: usize = 1 << 32;
pub const MEMORY_ADDRESS_MAXIMUM: u64 = 1 << 40;

/// Proof that mapping activity is frozen for a snapshot. Only the coordinator
/// that owns admission can construct this capability.
pub struct FrozenSnapshotAuthority {
    pub(crate) _private: (),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryMappingSnapshot {
    pub region: usize,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryCheckpointImage {
    pub version: u32,
    pub address_limit: u64,
    pub shared_limits: SharedLimits,
    pub shared: SharedStoreSnapshot,
    pub ledger: MemoryLedgerSnapshot,
    pub mappings: Vec<MemoryMappingSnapshot>,
}

pub trait MemoryHostRestore<H: crate::MappingHost>: Send {
    fn bind(&mut self, _: std::sync::Arc<crate::MappingCoordinator<H>>) -> Result<(), MemoryError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), MemoryError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), MemoryError>;
}

pub struct MemoryHostStage<H> {
    pub mapping: H,
    /// Exact replacement store used by both restored mappings and later
    /// memory-backed resource rebinding.
    pub shared: std::sync::Arc<SharedObjectStore>,
    pub restore: Box<dyn MemoryHostRestore<H>>,
}

pub trait MemoryCheckpointHost<H>: Send + Sync {
    fn address_limit(&self) -> u64;
    fn snapshot_mapping(
        &self,
        authority: &FrozenSnapshotAuthority,
        region: Region,
    ) -> Result<Vec<u8>, MemoryError>;

    fn stage(&self, image: &MemoryCheckpointImage) -> Result<MemoryHostStage<H>, MemoryError>;
}

impl MemoryCheckpointImage {
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.version != MEMORY_CHECKPOINT_VERSION
            || self.address_limit == 0
            || self.address_limit > MEMORY_ADDRESS_MAXIMUM
            || self.ledger.regions.len() > MEMORY_CHECKPOINT_REGION_MAXIMUM
            || self.mappings.len() != self.ledger.regions.len()
            || self.shared.objects.len() > self.shared_limits.objects
            || self.shared.generations.len() > self.shared_limits.objects
            || self.shared.generations.contains(&0)
            || self.shared_limits.objects == 0
            || self.shared_limits.object_bytes > self.shared_limits.total_bytes
        {
            return Err(MemoryError::InvariantViolation);
        }
        let mut total = 0_usize;
        let mut object_ids = BTreeSet::new();
        for object in &self.shared.objects {
            total = total
                .checked_add(object.bytes.len())
                .ok_or(MemoryError::Shared(SharedError::ResourceLimit))?;
            if object.bytes.len() > self.shared_limits.object_bytes
                || total > self.shared_limits.total_bytes
                || object.id.generation == 0
                || object.id.slot as usize >= self.shared_limits.objects
                || object.id.slot as usize >= self.shared.generations.len()
                || self.shared.generations[object.id.slot as usize] != object.id.generation
                || !object_ids.insert(object.id)
            {
                return Err(MemoryError::Shared(SharedError::InvalidArgument));
            }
        }
        MemoryLedger::restore(self.ledger.clone())?;
        let objects: BTreeMap<SharedObjectId, _> =
            self.shared.objects.iter().map(|object| (object.id, object)).collect();
        for region in &self.ledger.regions {
            let Backing::Shared(reference) = region.backing() else {
                continue;
            };
            let object = objects
                .get(&reference.object)
                .ok_or(MemoryError::Shared(SharedError::NotFound))?;
            let mapping_end = region
                .backing_offset()
                .checked_add(region.range().length())
                .ok_or(MemoryError::BackingOverflow)?;
            let reference_end = reference
                .offset
                .checked_add(reference.length)
                .ok_or(MemoryError::BackingOverflow)?;
            if mapping_end > reference.length || reference_end > object.bytes.len() as u64 {
                return Err(MemoryError::Shared(SharedError::Range));
            }
            if region.protection().contains(crate::Protection::WRITE)
                && object
                    .seals
                    .intersects(crate::SharedSeal::WRITE | crate::SharedSeal::FUTURE_WRITE)
            {
                return Err(MemoryError::Shared(SharedError::Sealed));
            }
        }
        if self.shared.objects.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            return Err(MemoryError::InvariantViolation);
        }
        let mut mapping_bytes = 0_usize;
        for (index, mapping) in self.mappings.iter().enumerate() {
            if mapping.region >= self.ledger.regions.len()
                || mapping.region != index
                || mapping.bytes.len() as u64 != self.ledger.regions[mapping.region].range().length()
            {
                return Err(MemoryError::InvariantViolation);
            }
            if self.ledger.regions[mapping.region].range().end().get() > self.address_limit {
                return Err(MemoryError::ResourceLimit);
            }
            mapping_bytes = mapping_bytes
                .checked_add(mapping.bytes.len())
                .ok_or(MemoryError::ResourceLimit)?;
            if mapping_bytes > MEMORY_CHECKPOINT_BYTES_MAXIMUM {
                return Err(MemoryError::ResourceLimit);
            }
        }
        Ok(())
    }
}
