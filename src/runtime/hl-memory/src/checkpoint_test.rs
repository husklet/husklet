use std::sync::Arc;

use hl_isa::{AddressRange, GuestAddress};

use crate::{
    Backing, MapRequest, MappingCoordinator, MappingHost, MemoryCheckpointHost, MemoryError, MemoryHostRestore,
    MemoryHostStage, Placement, Protection, SharedBackingRef, SharedError, SharedLimits, SharedObjectStore, SharedSeal,
};

#[derive(Debug)]
struct Mapping;

impl MappingHost for Mapping {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
}

struct HostRebind;

impl MemoryHostRestore<Mapping> for HostRebind {
    fn commit(&mut self) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), MemoryError> {
        Ok(())
    }
}

struct Host;

impl MemoryCheckpointHost<Mapping> for Host {
    fn address_limit(&self) -> u64 {
        65_536
    }
    fn snapshot_mapping(
        &self,
        _: &crate::FrozenSnapshotAuthority,
        region: crate::Region,
    ) -> Result<Vec<u8>, MemoryError> {
        Ok(vec![0; region.range().length() as usize])
    }

    fn stage(&self, image: &crate::MemoryCheckpointImage) -> Result<MemoryHostStage<Mapping>, MemoryError> {
        let shared = Arc::new(
            SharedObjectStore::restore(image.shared_limits, image.shared.clone()).map_err(MemoryError::Shared)?,
        );
        Ok(MemoryHostStage {
            mapping: Mapping,
            shared,
            restore: Box::new(HostRebind),
        })
    }
}

fn request(object: crate::SharedObjectId, address: u64, protection: Protection) -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length: 4096,
        alignment: 4096,
        protection,
        backing: Backing::Shared(SharedBackingRef {
            object,
            offset: 0,
            length: 8192,
            write_shared: true,
        }),
        backing_offset: 0,
    }
}

#[test]
fn aggregate_image_preserves() {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = shared.create(7, 8192).unwrap();
    let writer = shared.pin(object, true).unwrap();
    writer.write(2, b"rust").unwrap();
    drop(writer);
    shared
        .add_seals(object, SharedSeal::from_bits(SharedSeal::FUTURE_WRITE))
        .unwrap();
    let coordinator = MappingCoordinator::with_shared(Mapping, shared.clone());
    coordinator.map(request(object, 0x1000, Protection::READ)).unwrap();
    coordinator.map(request(object, 0x3000, Protection::READ)).unwrap();

    coordinator.freeze_checkpoint();
    shared.freeze_checkpoint();
    let image = coordinator.checkpoint_image(&Host).unwrap();
    shared.thaw_checkpoint();
    coordinator.thaw_checkpoint();

    assert_eq!(image.mappings.len(), 2);
    assert_eq!(image.shared.objects[0].bytes[2..6], *b"rust");
    assert!(image.shared.objects[0].seals.contains(SharedSeal::FUTURE_WRITE));
    let restored = Arc::new(SharedObjectStore::restore(image.shared_limits, image.shared.clone()).unwrap());
    let mappings = MappingCoordinator::restore(Mapping, restored.clone(), image.ledger).unwrap();
    assert_eq!(restored.pin_count(object), Ok(2));
    assert_eq!(mappings.ledger().regions().len(), 2);
}

#[test]
fn validation_rejects_overlap() {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = shared.create(1, 8192).unwrap();
    let coordinator = MappingCoordinator::with_shared(Mapping, shared.clone());
    coordinator.map(request(object, 0x1000, Protection::READ)).unwrap();
    coordinator.freeze_checkpoint();
    shared.freeze_checkpoint();
    let image = coordinator.checkpoint_image(&Host).unwrap();
    shared.thaw_checkpoint();
    coordinator.thaw_checkpoint();

    let mut overlap = image.clone();
    overlap.ledger.regions.push(overlap.ledger.regions[0]);
    overlap.mappings.push(crate::MemoryMappingSnapshot {
        region: 1,
        bytes: vec![0; 4096],
    });
    assert!(overlap.validate().is_err());

    let mut missing = image.clone();
    missing.shared.objects.clear();
    assert_eq!(missing.validate(), Err(MemoryError::Shared(SharedError::NotFound)));

    let writable_shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let writable_object = writable_shared.create(1, 8192).unwrap();
    let writable = MappingCoordinator::with_shared(Mapping, writable_shared.clone());
    writable
        .map(request(writable_object, 0x1000, Protection::WRITE))
        .unwrap();
    writable.freeze_checkpoint();
    writable_shared.freeze_checkpoint();
    let mut sealed = writable.checkpoint_image(&Host).unwrap();
    writable_shared.thaw_checkpoint();
    writable.thaw_checkpoint();
    sealed.shared.objects[0].seals = SharedSeal::from_bits(SharedSeal::WRITE);
    assert_eq!(sealed.validate(), Err(MemoryError::Shared(SharedError::Sealed)));

    let mut duplicate = image;
    duplicate.mappings[0].region = 1;
    assert_eq!(duplicate.validate(), Err(MemoryError::InvariantViolation));
}
