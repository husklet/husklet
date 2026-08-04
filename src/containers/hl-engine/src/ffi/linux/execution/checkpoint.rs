use std::sync::Arc;

use hl_memory::{
    Backing, MapRequest, MappingCoordinator, MappingHost, MemoryCheckpointHost, MemoryCheckpointImage, MemoryError,
    MemoryHostRestore, MemoryHostStage, Placement, Protection, Region, SharedObjectStore,
};

use super::{MappingHostAdapter, VirtualMemory, space::AddressSpace};

pub(super) struct Host {
    space: Arc<AddressSpace>,
}

impl Host {
    pub(super) fn new(space: Arc<AddressSpace>) -> Self {
        Self { space }
    }

    fn populate(
        adapter: &MappingHostAdapter,
        arena: &VirtualMemory,
        image: &MemoryCheckpointImage,
    ) -> Result<(), MemoryError> {
        let staging = Protection::READ.union(Protection::WRITE);
        for mapping in &image.mappings {
            let region = image.ledger.regions[mapping.region];
            if region.protection().contains(Protection::WRITE) && region.protection().contains(Protection::EXECUTE)
                || matches!(region.backing(), Backing::File { .. })
            {
                return Err(MemoryError::InvariantViolation);
            }
            let request = MapRequest {
                placement: Placement::Fixed(region.range().start()),
                length: region.range().length(),
                alignment: 4096,
                protection: staging,
                backing: region.backing(),
                backing_offset: region.backing_offset(),
            };
            let token = adapter.stage_map(region.range().start(), request)?;
            adapter.commit(&[token])?;
            arena
                .write(region.range().start().get(), &mapping.bytes)
                .map_err(|_| MemoryError::InvariantViolation)?;
            if region.protection() != staging {
                let token = adapter.stage_protect(region.range(), region.protection())?;
                adapter.commit(&[token])?;
            }
        }
        Ok(())
    }
}

impl MemoryCheckpointHost<MappingHostAdapter> for Host {
    fn address_limit(&self) -> u64 {
        self.space.arena().length() as u64
    }

    fn snapshot_mapping(
        &self,
        authority: &hl_memory::FrozenSnapshotAuthority,
        region: Region,
    ) -> Result<Vec<u8>, MemoryError> {
        let lease = self.space.lease();
        let mut bytes = vec![0; usize::try_from(region.range().length()).map_err(|_| MemoryError::ResourceLimit)?];
        lease
            .arena()
            .frozen_snapshot_read(authority, region.range().start().get(), &mut bytes, region.protection())
            .map_err(|_| MemoryError::InvariantViolation)?;
        Ok(bytes)
    }

    fn stage(&self, image: &MemoryCheckpointImage) -> Result<MemoryHostStage<MappingHostAdapter>, MemoryError> {
        let registry = Arc::new(super::super::shared_backing::Registry::default());
        let factory = Arc::new(super::super::shared_backing::Factory::new(Arc::clone(&registry)));
        let shared = Arc::new(
            SharedObjectStore::restore_with(image.shared_limits, image.shared.clone(), factory)
                .map_err(MemoryError::Shared)?,
        );
        let arena = Arc::new(
            VirtualMemory::reserve_in(
                self.space.arena().resource_context(),
                usize::try_from(image.address_limit).map_err(|_| MemoryError::ResourceLimit)?,
            )
            .map_err(|_| MemoryError::ResourceLimit)?
            .with_shared_store(Arc::clone(&shared))
            .with_shared_backings(registry),
        );
        let adapter = MappingHostAdapter::new(Arc::clone(&arena));
        Self::populate(&adapter, &arena, image)?;
        Ok(MemoryHostStage {
            mapping: adapter,
            shared,
            restore: Box::new(SpaceTransaction {
                space: Arc::clone(&self.space),
                expected: self.space.lease().generation(),
                arena,
                mappings: None,
                retired: None,
            }),
        })
    }
}

struct SpaceTransaction {
    space: Arc<AddressSpace>,
    expected: u64,
    arena: Arc<VirtualMemory>,
    mappings: Option<Arc<MappingCoordinator<MappingHostAdapter>>>,
    retired: Option<super::space::SpaceLease>,
}

impl MemoryHostRestore<MappingHostAdapter> for SpaceTransaction {
    fn bind(&mut self, mappings: Arc<MappingCoordinator<MappingHostAdapter>>) -> Result<(), MemoryError> {
        if self.mappings.replace(mappings).is_some() {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), MemoryError> {
        let mappings = self.mappings.as_ref().ok_or(MemoryError::InvariantViolation)?;
        self.retired = Some(
            self.space
                .replace(self.expected, Arc::clone(&self.arena), Arc::clone(mappings))
                .map_err(|_| MemoryError::InvariantViolation)?,
        );
        Ok(())
    }

    fn rollback(&mut self) {
        if let Some(retired) = self.retired.take() {
            let _ = self
                .space
                .replace(self.expected.saturating_add(1), retired.arena(), retired.mappings());
        }
    }

    fn resume(&mut self) -> Result<(), MemoryError> {
        Ok(())
    }
}
