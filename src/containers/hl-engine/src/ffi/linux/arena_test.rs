use std::sync::Arc;
use std::{fs, process};

use hl_isa::{AddressRange, GuestAddress};
use hl_loader::{
    ImageProtectionRegistry, MappingKind, MappingPlacement, Protection as ImageProtection, TransactionalAddressSpace,
};
use hl_memory::{
    AtomicU32Write, AtomicWriteBatchHost, Backing, FileIdentity, MapRequest, MappingHost, MemoryAccessHost, Placement,
    Protection,
};

use super::arena::{Ledger, Operation};
use super::{AddressSpaceAdapter, MappingHostAdapter, VirtualMemory};

const PAGE: u64 = 4096;

fn request(protection: Protection) -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0)),
        length: PAGE,
        alignment: PAGE,
        protection,
        backing: Backing::Anonymous {
            identity: 1,
            shared: false,
        },
        backing_offset: 0,
    }
}

fn fixed_request(address: u64, length: u64, backing_offset: u64, protection: Protection) -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length,
        alignment: PAGE,
        protection,
        backing: Backing::Anonymous {
            identity: address + 1,
            shared: false,
        },
        backing_offset,
    }
}

#[test]
fn ledger_offsets_survive() {
    let reference = hl_memory::SharedBackingRef {
        object: hl_memory::SharedObjectId { slot: 7, generation: 3 },
        offset: 128,
        length: PAGE * 4,
        write_shared: true,
    };
    let request = MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0)),
        length: PAGE * 3,
        alignment: PAGE,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Shared(reference),
        backing_offset: 256,
    };
    let mut ledger = Ledger::default();
    ledger.apply(Operation::Map(0, request)).unwrap();
    let before = ledger.reservation(PAGE * 2 + 64).unwrap().0;

    ledger.apply(Operation::Protect(PAGE, PAGE, Protection::READ)).unwrap();
    assert_eq!(ledger.reservation(PAGE * 2 + 64).unwrap().0, before);

    ledger.apply(Operation::Unmap(0, PAGE)).unwrap();
    assert_eq!(ledger.reservation(PAGE * 2 + 64).unwrap().0, before);
}

#[test]
fn hole_unmap_succeeds() {
    let arena = VirtualMemory::reserve((PAGE * 3) as usize).unwrap();
    let token = arena.stage(Operation::Unmap(PAGE, PAGE)).unwrap();
    arena.commit(&[token]).unwrap();
    assert!(arena.state.lock().unwrap().mappings.reservation(PAGE).is_err());
}

#[test]
fn middle_unmap_splits() {
    let arena = VirtualMemory::reserve((PAGE * 3) as usize).unwrap();
    let mapping = fixed_request(0, PAGE * 3, PAGE * 4, Protection::READ);
    let map = arena.stage(Operation::Map(0, mapping)).unwrap();
    arena.commit(&[map]).unwrap();
    let left = arena.state.lock().unwrap().mappings.reservation(64).unwrap().0;
    let right = arena
        .state
        .lock()
        .unwrap()
        .mappings
        .reservation(PAGE * 2 + 64)
        .unwrap()
        .0;

    let unmap = arena.stage(Operation::Unmap(PAGE, PAGE)).unwrap();
    arena.commit(&[unmap]).unwrap();
    let ledger = &arena.state.lock().unwrap().mappings;
    assert_eq!(ledger.reservation(64).unwrap().0, left);
    assert!(ledger.reservation(PAGE).is_err());
    assert_eq!(ledger.reservation(PAGE * 2 + 64).unwrap().0, right);
}

#[test]
fn mixed_holes_unmap() {
    let arena = VirtualMemory::reserve((PAGE * 5) as usize).unwrap();
    for address in [PAGE, PAGE * 3] {
        let map = arena
            .stage(Operation::Map(
                address,
                fixed_request(address, PAGE, address, Protection::READ),
            ))
            .unwrap();
        arena.commit(&[map]).unwrap();
    }

    let unmap = arena.stage(Operation::Unmap(0, PAGE * 5)).unwrap();
    arena.commit(&[unmap]).unwrap();
    let ledger = &arena.state.lock().unwrap().mappings;
    for address in [0, PAGE, PAGE * 2, PAGE * 3, PAGE * 4] {
        assert!(ledger.reservation(address).is_err());
    }
}

#[test]
fn unmap_rollback_exact() {
    let path = std::path::PathBuf::from(format!("/tmp/hl-unmap-rollback-{}", process::id()));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.set_len(PAGE * 6).unwrap();
    let identity = FileIdentity { device: 31, object: 47 };
    let arena = VirtualMemory::reserve((PAGE * 6) as usize).unwrap();
    arena.register_file(identity, &file).unwrap();
    let mappings = [
        (PAGE, PAGE, Protection::READ.union(Protection::WRITE)),
        (PAGE * 3, PAGE * 4, Protection::READ),
    ];
    for (address, backing_offset, protection) in mappings {
        let request = MapRequest {
            backing: Backing::File { identity, shared: true },
            backing_offset,
            ..fixed_request(address, PAGE, backing_offset, protection)
        };
        let map = arena.stage(Operation::Map(address, request)).unwrap();
        arena.commit(&[map]).unwrap();
    }
    arena.write(PAGE, b"before").unwrap();
    let before = mappings.map(|(address, _, _)| {
        arena
            .state
            .lock()
            .unwrap()
            .mappings
            .reservation(address + 64)
            .unwrap()
            .0
    });

    let unmap = arena.stage(Operation::Unmap(0, PAGE * 5)).unwrap();
    let later = arena
        .stage(Operation::Map(
            PAGE * 5,
            fixed_request(PAGE * 5, PAGE, 0, Protection::READ),
        ))
        .unwrap();
    arena.inject_failures(&[2]);
    assert!(arena.commit(&[unmap, later]).is_err());
    arena.inject_failures(&[]);

    {
        let state = arena.state.lock().unwrap();
        assert_eq!(state.mappings.reservation(PAGE + 64).unwrap().0, before[0]);
        assert_eq!(state.mappings.reservation(PAGE * 3 + 64).unwrap().0, before[1]);
        for address in [0, PAGE * 2, PAGE * 4, PAGE * 5] {
            assert!(state.mappings.reservation(address).is_err());
        }
    }
    let mut observed = [0; 6];
    arena.read(PAGE, &mut observed).unwrap();
    assert_eq!(&observed, b"before");
    assert!(arena.write(PAGE * 3, b"x").is_err());
    fs::remove_file(path).unwrap();
}

#[test]
fn protect_hole_fails() {
    let arena = VirtualMemory::reserve((PAGE * 3) as usize).unwrap();
    for address in [0, PAGE * 2] {
        let map = arena
            .stage(Operation::Map(
                address,
                fixed_request(address, PAGE, address, Protection::READ),
            ))
            .unwrap();
        arena.commit(&[map]).unwrap();
    }
    let before = [0, PAGE * 2].map(|address| {
        arena
            .state
            .lock()
            .unwrap()
            .mappings
            .reservation(address + 64)
            .unwrap()
            .0
    });

    let protect = arena.stage(Operation::Protect(0, PAGE * 3, Protection::WRITE)).unwrap();
    assert!(arena.commit(&[protect]).is_err());
    let ledger = &arena.state.lock().unwrap().mappings;
    assert_eq!(ledger.reservation(64).unwrap().0, before[0]);
    assert!(ledger.reservation(PAGE).is_err());
    assert_eq!(ledger.reservation(PAGE * 2 + 64).unwrap().0, before[1]);
}

#[test]
fn arena_projects_wx() {
    assert!(VirtualMemory::reserve(0).is_err());
    assert!(VirtualMemory::reserve(1).is_err());
    assert!(VirtualMemory::reserve(usize::MAX).is_err());
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let wx = Protection::WRITE.union(Protection::EXECUTE);
    let token = host.stage_map(GuestAddress::new(0), request(wx)).unwrap();
    host.commit(&[token]).unwrap();
}

#[test]
fn shared_registry_required() {
    let shared = Backing::Shared(hl_memory::SharedBackingRef {
        object: hl_memory::SharedObjectId { slot: 1, generation: 1 },
        offset: 0,
        length: PAGE,
        write_shared: true,
    });
    let mapping = MapRequest {
        backing: shared,
        ..request(Protection::READ)
    };
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    assert!(host.commit(&[token]).is_err());

    let registry = Arc::new(super::shared_backing::Registry::default());
    let arena = Arc::new(
        VirtualMemory::reserve(PAGE as usize)
            .unwrap()
            .with_shared_backings(registry),
    );
    let host = MappingHostAdapter::new(arena);
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    assert!(host.commit(&[token]).is_err());
}

#[test]
fn shared_mapping_coherent() {
    let registry = Arc::new(super::shared_backing::Registry::default());
    let factory = Arc::new(super::shared_backing::Factory::new(Arc::clone(&registry)));
    let store =
        Arc::new(hl_memory::SharedObjectStore::with_factory(hl_memory::SharedLimits::default(), factory).unwrap());
    let object = store.create(7, PAGE as usize).unwrap();
    let pin = store.pin(object, true).unwrap();
    let arena = Arc::new(
        VirtualMemory::reserve(PAGE as usize)
            .unwrap()
            .with_shared_store(store)
            .with_shared_backings(registry),
    );
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    let mapping = MapRequest {
        backing: Backing::Shared(hl_memory::SharedBackingRef {
            object,
            offset: 0,
            length: PAGE,
            write_shared: true,
        }),
        protection: Protection::READ.union(Protection::WRITE),
        ..request(Protection::READ)
    };
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    host.commit(&[token]).unwrap();

    pin.write(0, b"backing").unwrap();
    let mut observed = [0_u8; 7];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 7).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(&observed, b"backing");

    let write = host
        .prepare_write(AddressRange::nonempty(GuestAddress::new(0), 7).unwrap())
        .unwrap();
    host.commit_write(write, b"mapping").unwrap();
    pin.read(0, &mut observed).unwrap();
    assert_eq!(&observed, b"mapping");
}

#[test]
fn outside_stage_atomic() {
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let mut outside = request(Protection::READ);
    outside.placement = Placement::Fixed(GuestAddress::new(PAGE));
    assert!(host.stage_map(GuestAddress::new(PAGE), outside).is_err());

    let token = host.stage_map(GuestAddress::new(0), request(Protection::READ)).unwrap();
    host.commit(&[token]).unwrap();
    let mut observed = [0; 1];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 1).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
}

#[test]
fn host_read_preserves_execute_authority() {
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let token = host
        .stage_map(GuestAddress::new(0), request(Protection::EXECUTE))
        .unwrap();
    host.commit(&[token]).unwrap();
    let range = AddressRange::nonempty(GuestAddress::new(0), 1).unwrap();
    let mut observed = [0xa5];

    host.read(range, &mut observed, Protection::EXECUTE).unwrap();

    assert_eq!(observed, [0]);
    assert_eq!(
        host.read(range, &mut observed, Protection::READ),
        Err(hl_memory::MemoryError::NoAddressSpace),
    );
}

#[test]
fn logical_unmap_then() {
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let token = host
        .stage_map(GuestAddress::new(0), request(Protection::READ.union(Protection::WRITE)))
        .unwrap();
    host.commit(&[token]).unwrap();
    let range = AddressRange::nonempty(GuestAddress::new(0), PAGE).unwrap();
    let write = host.prepare_write(range).unwrap();
    host.commit_write(write, &[7; PAGE as usize]).unwrap();
    let token = host.stage_unmap(range).unwrap();
    host.commit(&[token]).unwrap();
    let token = host
        .stage_map(GuestAddress::new(0), request(Protection::READ.union(Protection::WRITE)))
        .unwrap();
    host.commit(&[token]).unwrap();
    let mut observed = [1; 16];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 16).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [0; 16]);
}

#[test]
fn loader_commit_publishes() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 2) as usize).unwrap());
    let mut loader = AddressSpaceAdapter::new(Arc::clone(&arena));
    let mapping = loader
        .reserve(MappingKind::MainImage, PAGE, MappingPlacement::Fixed(0))
        .unwrap();
    loader.stage_write(mapping.token(), 8, &[1, 2, 3, 4]).unwrap();
    loader
        .stage_protection(
            mapping.token(),
            0,
            PAGE,
            ImageProtection::from_bits(ImageProtection::READ),
        )
        .unwrap();
    loader.commit(&[*mapping.token()]).unwrap();
    let host = MappingHostAdapter::new(arena);
    let mut observed = [0; 4];
    host.read(
        AddressRange::nonempty(GuestAddress::new(8), 4).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [1, 2, 3, 4]);
}

#[test]
fn loader_protects_independent() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 3) as usize).unwrap());
    let mut loader = AddressSpaceAdapter::new(Arc::clone(&arena));
    let mapping = loader
        .reserve(MappingKind::MainImage, PAGE * 3, MappingPlacement::Fixed(0))
        .unwrap();
    loader.stage_write(mapping.token(), 0, &[1]).unwrap();
    loader.stage_write(mapping.token(), PAGE * 2, &[2]).unwrap();
    loader
        .stage_protection(
            mapping.token(),
            0,
            PAGE,
            ImageProtection::from_bits(ImageProtection::READ),
        )
        .unwrap();
    loader
        .stage_protection(
            mapping.token(),
            PAGE * 2,
            PAGE,
            ImageProtection::from_bits(ImageProtection::READ | ImageProtection::EXECUTE),
        )
        .unwrap();
    loader.commit(&[*mapping.token()]).unwrap();
    let host = MappingHostAdapter::new(arena);
    let mut observed = [0];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 1).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [1]);
    assert!(
        host.read(
            AddressRange::nonempty(GuestAddress::new(PAGE), 1).unwrap(),
            &mut observed,
            Protection::READ,
        )
        .is_err()
    );
    host.read(
        AddressRange::nonempty(GuestAddress::new(PAGE * 2), 1).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [2]);
}

#[test]
fn failed_partial_protection() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 3) as usize).unwrap());
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    let mut mapping = request(Protection::READ.union(Protection::WRITE));
    mapping.length = PAGE * 3;
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    host.commit(&[token]).unwrap();
    arena.inject_failures(&[2]);
    let tokens = [
        host.stage_protect(
            AddressRange::nonempty(GuestAddress::new(PAGE), PAGE).unwrap(),
            Protection::READ,
        )
        .unwrap(),
        host.stage_protect(
            AddressRange::nonempty(GuestAddress::new(PAGE * 2), PAGE).unwrap(),
            Protection::READ,
        )
        .unwrap(),
    ];
    assert!(host.commit(&tokens).is_err());
    arena.inject_failures(&[]);
    let write = host
        .prepare_write(AddressRange::nonempty(GuestAddress::new(0), PAGE * 3).unwrap())
        .unwrap();
    host.commit_write(write, &[7; (PAGE * 3) as usize]).unwrap();
}

#[test]
fn split_mapping_unmaps() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 3) as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let mut mapping = request(Protection::READ.union(Protection::WRITE));
    mapping.length = PAGE * 3;
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    host.commit(&[token]).unwrap();
    let middle = AddressRange::nonempty(GuestAddress::new(PAGE), PAGE).unwrap();
    let token = host.stage_protect(middle, Protection::READ).unwrap();
    host.commit(&[token]).unwrap();
    let complete = AddressRange::nonempty(GuestAddress::new(0), PAGE * 3).unwrap();
    let token = host.stage_unmap(complete).unwrap();
    host.commit(&[token]).unwrap();
    let token = host.stage_map(GuestAddress::new(0), mapping).unwrap();
    host.commit(&[token]).unwrap();
}

#[test]
fn file_replacement() {
    let path = std::path::PathBuf::from(format!("/tmp/hl-map-replace-{}", process::id(),));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.set_len(PAGE).unwrap();
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let identity = FileIdentity { device: 7, object: 11 };
    arena.register_file(identity, &file).unwrap();
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    let file_map = MapRequest {
        backing: Backing::File {
            identity,
            shared: false,
        },
        ..request(Protection::READ)
    };
    let token = host.stage_map(GuestAddress::new(0), file_map).unwrap();
    host.commit(&[token]).unwrap();
    file.set_len(0).unwrap();

    let replacement = request(Protection::READ.union(Protection::WRITE));
    let token = host.stage_map(GuestAddress::new(0), replacement).unwrap();
    host.commit(&[token]).unwrap();
    let write = host
        .prepare_write(AddressRange::nonempty(GuestAddress::new(0), 1).unwrap())
        .unwrap();
    host.commit_write(write, &[0x5a]).unwrap();
    let mut observed = [0];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 1).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [0x5a]);
    fs::remove_file(path).unwrap();
}

#[test]
fn failed_batch_compensates() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 2) as usize).unwrap());
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    for address in [0, PAGE] {
        let mut mapping = request(Protection::READ);
        mapping.placement = Placement::Fixed(GuestAddress::new(address));
        let token = host.stage_map(GuestAddress::new(address), mapping).unwrap();
        host.commit(&[token]).unwrap();
    }
    arena.inject_failures(&[2]);
    let first = AddressRange::nonempty(GuestAddress::new(0), PAGE).unwrap();
    let second = AddressRange::nonempty(GuestAddress::new(PAGE), PAGE).unwrap();
    let tokens = [
        host.stage_protect(first, Protection::WRITE).unwrap(),
        host.stage_protect(second, Protection::WRITE).unwrap(),
    ];
    assert!(host.commit(&tokens).is_err());
    arena.inject_failures(&[]);
    let mut byte = [0];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 1).unwrap(),
        &mut byte,
        Protection::READ,
    )
    .unwrap();
}

#[test]
fn failed_compensation_poison() {
    let arena = Arc::new(VirtualMemory::reserve((PAGE * 2) as usize).unwrap());
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    for address in [0, PAGE] {
        let mut mapping = request(Protection::READ);
        mapping.placement = Placement::Fixed(GuestAddress::new(address));
        let token = host.stage_map(GuestAddress::new(address), mapping).unwrap();
        host.commit(&[token]).unwrap();
    }
    arena.inject_failures(&[2, 3]);
    let tokens = [
        host.stage_protect(
            AddressRange::nonempty(GuestAddress::new(0), PAGE).unwrap(),
            Protection::WRITE,
        )
        .unwrap(),
        host.stage_protect(
            AddressRange::nonempty(GuestAddress::new(PAGE), PAGE).unwrap(),
            Protection::WRITE,
        )
        .unwrap(),
    ];
    assert!(host.commit(&tokens).is_err());
    assert!(host.stage_map(GuestAddress::new(0), request(Protection::READ)).is_err());
}

#[test]
fn loader_failure_does() {
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let mut loader = AddressSpaceAdapter::new(Arc::clone(&arena));
    let mapping = loader
        .reserve(MappingKind::MainImage, PAGE, MappingPlacement::Fixed(0))
        .unwrap();
    loader.stage_executable(mapping.token(), 0, PAGE).unwrap();
    loader.stage_guest_access(mapping.token(), 0, PAGE, true).unwrap();
    arena.inject_failures(&[1]);
    assert!(loader.commit(&[*mapping.token()]).is_err());
    assert_eq!(loader.metadata_count(), 0);
}

#[test]
fn atomic_later_mismatch() {
    let arena = Arc::new(VirtualMemory::reserve(PAGE as usize).unwrap());
    let host = MappingHostAdapter::new(arena);
    let token = host
        .stage_map(GuestAddress::new(0), request(Protection::READ.union(Protection::WRITE)))
        .unwrap();
    host.commit(&[token]).unwrap();
    let token = host
        .prepare_u32_batch(&[
            AtomicU32Write {
                address: GuestAddress::new(0),
                expected: 0,
                replacement: 1,
            },
            AtomicU32Write {
                address: GuestAddress::new(4),
                expected: 9,
                replacement: 2,
            },
        ])
        .unwrap();
    assert!(host.commit_u32_batch(token).is_err());
    let mut observed = [1; 4];
    host.read(
        AddressRange::nonempty(GuestAddress::new(0), 4).unwrap(),
        &mut observed,
        Protection::READ,
    )
    .unwrap();
    assert_eq!(observed, [0; 4]);
}
