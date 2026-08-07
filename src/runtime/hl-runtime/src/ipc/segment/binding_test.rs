#![allow(clippy::vec_init_then_push)]

use std::sync::Arc;

use hl_ipc::{AttachPlan, SharedMemoryId};
use hl_isa::GuestAddress;
use hl_memory::{Backing, MappingCoordinator, SharedBackingRef, SharedLimits, SharedObjectStore, TestMappingHost};

use crate::{ForkBinding, MemoryBinding, MemoryMappings, MemoryPort};

struct Fixture {
    parent: MemoryMappings<TestMappingHost>,
    child: MemoryMappings<TestMappingHost>,
    child_coordinator: Arc<MappingCoordinator<TestMappingHost>>,
    ledger: Vec<hl_memory::Region>,
    binding: MemoryBinding,
}

impl Fixture {
    fn new() -> Self {
        let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let object = store.create(1, 4096).unwrap();
        let coordinator = Arc::new(MappingCoordinator::with_shared(TestMappingHost, store));
        let parent = MemoryMappings::new(Arc::clone(&coordinator));
        let address = parent
            .map(
                AttachPlan {
                    segment: SharedMemoryId { slot: 1, generation: 1 },
                    backing: SharedBackingRef {
                        object,
                        offset: 0,
                        length: 4096,
                        write_shared: true,
                    },
                    read_only: false,
                    executable: false,
                    round_address: false,
                    replace: false,
                },
                GuestAddress::new(0x4000),
            )
            .unwrap();
        parent.bind(address, 11).unwrap();
        let binding = parent.bindings().unwrap()[0];
        let child_coordinator = Arc::new(coordinator.fork_restore(TestMappingHost).unwrap());
        let ledger = child_coordinator.ledger().regions();
        let child = MemoryMappings::new(Arc::clone(&child_coordinator));
        Self {
            parent,
            child,
            child_coordinator,
            ledger,
            binding,
        }
    }
}

#[test]
fn prepared_child_ledger() {
    let fixture = Fixture::new();
    let mut binding = fixture.binding;
    binding.attachment = 21;
    let prepared = fixture.child.prepare_restore_bindings(&[binding]).unwrap();
    assert!(fixture.child.bindings().unwrap().is_empty());
    let committed = prepared.commit().unwrap();
    assert_eq!(fixture.child.bindings(), Ok(vec![binding]));
    committed.rollback().unwrap();
    assert!(fixture.child.bindings().unwrap().is_empty());
    assert_eq!(fixture.child_coordinator.ledger().regions(), fixture.ledger);
    assert_eq!(fixture.parent.bindings(), Ok(vec![fixture.binding]));
}

#[test]
fn invalid_child_publication() {
    let fixture = Fixture::new();
    let valid = fixture.binding;
    let mut cases = Vec::new();
    cases.push(MemoryBinding { attachment: 0, ..valid });
    cases.push(MemoryBinding { length: 8192, ..valid });
    cases.push(MemoryBinding {
        address: GuestAddress::new(0x8000),
        ..valid
    });
    for binding in cases {
        assert!(fixture.child.prepare_restore_bindings(&[binding]).is_err());
        assert!(fixture.child.bindings().unwrap().is_empty());
        assert_eq!(fixture.child_coordinator.ledger().regions(), fixture.ledger);
    }
    assert!(fixture.child.prepare_restore_bindings(&[valid, valid]).is_err());
    let Backing::Shared(mut backing) = fixture.ledger[0].backing() else {
        panic!();
    };
    backing.length += 4096;
    assert!(
        fixture
            .child
            .prepare_fork_bindings(&[ForkBinding {
                binding: valid,
                backing,
            }])
            .is_err()
    );
    assert!(fixture.child.bindings().unwrap().is_empty());
}

#[test]
fn binding_state_overwrite() {
    let fixture = Fixture::new();
    let mut planned = fixture.binding;
    planned.attachment = 21;
    let prepared = fixture.child.prepare_restore_bindings(&[planned]).unwrap();
    fixture.child.restore_bindings(&[fixture.binding]).unwrap();
    assert!(prepared.commit().is_err());
    assert_eq!(fixture.child.bindings(), Ok(vec![fixture.binding]));
    assert_eq!(fixture.child_coordinator.ledger().regions(), fixture.ledger);
}
