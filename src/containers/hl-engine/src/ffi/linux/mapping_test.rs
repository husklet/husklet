use std::{fs, process, sync::Arc};

use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{
    Backing, BackingChange, BackingChangeFlags, FileIdentity, HostProjection, MapRequest, MappingCoordinator,
    MappingHost, MemoryAccessHost, Placement, Protection,
};
use hl_runtime::{MemoryExit, RuntimeAssembly, RuntimeDomain};
use hl_task::{ProcessCredentials, ProcessLimits};

use super::mapping::Projection;
use super::{MappingHostAdapter, VirtualMemory};

struct Fixture {
    arena: Arc<VirtualMemory>,
    memory: Arc<MappingCoordinator<MappingHostAdapter>>,
}

impl Fixture {
    fn new() -> Self {
        let arena = Arc::new(VirtualMemory::reserve(4096).unwrap());
        let memory = Arc::new(MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena))));
        memory
            .map(MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        let write = memory.prepare_write(GuestAddress::new(0), 4).unwrap();
        memory.commit_write(write, b"exit").unwrap();
        Self { arena, memory }
    }
}

#[test]
fn direct_projection_admits_mutation_after_drop() {
    let arena = Arc::new(VirtualMemory::reserve(8192).unwrap());
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    let range = AddressRange::nonempty(GuestAddress::new(0), 4096).unwrap();
    let projection = host.project(range).unwrap();
    assert_ne!(projection.storage_address(), 0);
    let request = MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ,
        backing: Backing::Anonymous {
            identity: 90,
            shared: false,
        },
        backing_offset: 0,
    };
    assert!(host.stage_map(GuestAddress::new(0), request).is_err());
    let separate = host.stage_map(GuestAddress::new(4096), request).unwrap();
    host.rollback(separate);
    drop(projection);
    let admitted = host.stage_map(GuestAddress::new(0), request).unwrap();
    host.rollback(admitted);
}

#[test]
fn sparse_candidate_publishes_only_after_commit() {
    let path = std::path::PathBuf::from(format!("/tmp/hl-sparse-stage-{}", process::id()));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.set_len(8192).unwrap();
    let identity = FileIdentity { device: 81, object: 82 };
    let arena = Arc::new(VirtualMemory::reserve(8192).unwrap());
    arena.register_file(identity, &file).unwrap();
    let host = MappingHostAdapter::new(Arc::clone(&arena));
    let request = MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::File { identity, shared: true },
        backing_offset: 0,
    };
    let range = AddressRange::nonempty(GuestAddress::new(0), 4).unwrap();
    assert!(host.project_aperture().unwrap().is_some());
    let first = host.stage_map(GuestAddress::new(0), request).unwrap();
    assert!(matches!(host.project(range).unwrap(), Projection::Direct(_)));
    host.rollback(first);
    assert!(matches!(host.project(range).unwrap(), Projection::Direct(_)));

    let first = host.stage_map(GuestAddress::new(0), request).unwrap();
    let second = host.stage_map(GuestAddress::new(4096), request).unwrap();
    host.rollback(second);
    host.rollback(first);
    assert!(matches!(host.project(range).unwrap(), Projection::Direct(_)));

    let failed = host.stage_map(GuestAddress::new(0), request).unwrap();
    arena.inject_failures(&[1]);
    assert!(host.commit(&[failed]).is_err());
    host.rollback(failed);
    arena.inject_failures(&[]);
    assert!(matches!(host.project(range).unwrap(), Projection::Direct(_)));

    let committed = host.stage_map(GuestAddress::new(0), request).unwrap();
    host.commit(&[committed]).unwrap();
    assert!(matches!(host.project(range).unwrap(), Projection::Backing(_)));
    assert!(host.project_aperture().unwrap().is_none());
    fs::remove_file(path).unwrap();
}

#[test]
fn exit_rollback() {
    let fixture = Fixture::new();
    let mut exit = fixture.memory.prepare_exit().unwrap();
    exit.publish().unwrap();
    exit.rollback().unwrap();
    let mut bytes = [0; 4];
    fixture.arena.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"exit");
}

#[test]
fn exit_finish() {
    let fixture = Fixture::new();
    let mut exit = fixture.memory.prepare_exit().unwrap();
    exit.publish().unwrap();
    exit.finish();
    assert!(fixture.arena.read(0, &mut [0; 1]).is_err());
}

#[test]
fn assembly_memory_exit() {
    let fixture = Fixture::new();
    let assembly = RuntimeAssembly::new(hl_runtime::HostCapacityPlan::default()).unwrap();
    let (process, thread) = assembly
        .tasks()
        .create_init(ProcessCredentials::new(0, 0, &[], 4).unwrap(), ProcessLimits::empty())
        .unwrap();
    assembly
        .install_memory(Arc::new(MemoryExit::new(Arc::clone(&fixture.memory))))
        .unwrap();
    assert_eq!(assembly.require(RuntimeDomain::Memory), Ok(()));
    let participant = assembly.memory().unwrap();
    let mut exit = participant.prepare(process, &[thread]).unwrap();
    exit.publish().unwrap();
    assert!(fixture.memory.ledger().regions().is_empty());
    exit.rollback();
    assert!(!fixture.memory.snapshot().regions.is_empty());
    let mut bytes = [0; 4];
    fixture.arena.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"exit");

    let mut exit = participant.prepare(process, &[thread]).unwrap();
    exit.publish().unwrap();
    exit.finish();
    assert!(fixture.memory.ledger().regions().is_empty());
    assert!(fixture.arena.read(0, &mut [0; 1]).is_err());
}

#[test]
fn external_resize() {
    let path = std::path::PathBuf::from(format!("/tmp/hl-backing-change-{}", process::id(),));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.set_len(8192).unwrap();
    let identity = FileIdentity { device: 7, object: 19 };
    let arena = Arc::new(VirtualMemory::reserve(8192).unwrap());
    arena.register_file(identity, &file).unwrap();
    let memory = MappingCoordinator::new(MappingHostAdapter::new(Arc::clone(&arena)));
    memory
        .map(MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0)),
            length: 8192,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::File { identity, shared: true },
            backing_offset: 0,
        })
        .unwrap();

    file.set_len(4096).unwrap();
    let shrink = BackingChange {
        identity,
        old_size: 8192,
        new_size: 4096,
        flags: BackingChangeFlags::SIZE,
    };
    assert_eq!(memory.backing_changed(shrink), Ok(1));
    assert!(
        memory
            .read(GuestAddress::new(4096), &mut [0; 1], Protection::READ,)
            .is_err()
    );

    file.set_len(8192).unwrap();
    let regrow = BackingChange {
        identity,
        old_size: 4096,
        new_size: 8192,
        flags: BackingChangeFlags::SIZE,
    };
    assert_eq!(memory.backing_changed(regrow), Ok(1));
    assert!(
        memory
            .read(GuestAddress::new(4096), &mut [0; 1], Protection::READ,)
            .is_ok()
    );
    fs::remove_file(path).unwrap();
}
