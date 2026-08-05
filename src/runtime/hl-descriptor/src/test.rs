use crate::{
    DescriptorError, DescriptorFlags, DescriptorTable, ExactDuplicate, ObjectError, OpenFileDescription,
    OperationContext, StatusFlags,
};
use std::collections::BTreeMap;
use std::io::IoSlice;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct TestDescription;

impl OpenFileDescription for TestDescription {}

#[derive(Debug, Default)]
struct ScalarWriter(Mutex<Vec<u8>>);

impl OpenFileDescription for ScalarWriter {
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.0.lock().unwrap().extend_from_slice(input);
        Ok(input.len())
    }
}

#[test]
fn scalar_writers_accept_vectored_output() {
    let output = Arc::new(ScalarWriter::default());
    let table = DescriptorTable::new(2).unwrap();
    let descriptor = table
        .commit(
            table.reserve_exact(1).unwrap(),
            output.clone(),
            StatusFlags::from_bits(1),
            DescriptorFlags::default(),
        )
        .unwrap();
    let lease = table.pin(descriptor).unwrap();
    let vectors = [IoSlice::new(b""), IoSlice::new(b"external grep output\n")];

    assert_eq!(
        lease.write_vector_context(
            &vectors,
            OperationContext {
                actor: None,
                cancellation: None,
            },
        ),
        Ok(vectors[1].len()),
    );
    assert_eq!(&*output.0.lock().unwrap(), b"external grep output\n");
}

#[derive(Debug, Default)]
struct LifecycleDescription {
    retired: AtomicUsize,
    closed: AtomicUsize,
}

impl OpenFileDescription for LifecycleDescription {
    fn retire(&self) {
        self.retired.fetch_add(1, Ordering::Relaxed);
    }

    fn close(&self) {
        self.closed.fetch_add(1, Ordering::Relaxed);
    }
}

fn description() -> Arc<dyn OpenFileDescription> {
    Arc::new(TestDescription)
}

fn close_on_exec(model: &mut BTreeMap<i32, (u64, DescriptorFlags)>) -> Vec<i32> {
    let closed: Vec<i32> = model
        .iter()
        .filter_map(|(number, (_, flags))| flags.closes_on_exec().then_some(*number))
        .collect();
    for number in &closed {
        model.remove(number);
    }
    closed
}

fn lowest_free_model(model: &BTreeMap<i32, (u64, DescriptorFlags)>) -> Option<i32> {
    (0..32).find(|number| !model.contains_key(number))
}

#[test]
fn allocation_uses_lowest() {
    let table = DescriptorTable::new(8).unwrap();
    assert_eq!(table.install(3, description(), DescriptorFlags::default()).unwrap(), 3);
    assert_eq!(table.install(3, description(), DescriptorFlags::default()).unwrap(), 4);
    table.close(3).unwrap();
    assert_eq!(table.install(3, description(), DescriptorFlags::default()).unwrap(), 3);
}

#[test]
fn duplicate_shares_description() {
    let table = DescriptorTable::new(8).unwrap();
    let original = description();
    let source = table
        .install(
            0,
            original.clone(),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    let duplicate = table.duplicate(source, 0, DescriptorFlags::default()).unwrap();

    assert!(Arc::ptr_eq(
        table.lookup(source).unwrap().description(),
        table.lookup(duplicate).unwrap().description()
    ));
    assert!(table.lookup(source).unwrap().flags().closes_on_exec());
    assert!(!table.lookup(duplicate).unwrap().flags().closes_on_exec());
}

#[test]
fn fork_preserves_semantics() {
    let parent = DescriptorTable::new(8).unwrap();
    let descriptor = parent.install(0, description(), DescriptorFlags::default()).unwrap();
    parent.set_offset(descriptor, 17).unwrap();
    let child = parent.fork();

    child.set_offset(descriptor, 31).unwrap();
    child
        .set_flags(descriptor, DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC))
        .unwrap();

    let parent_view = parent.snapshot(descriptor).unwrap();
    let child_view = child.snapshot(descriptor).unwrap();
    assert_eq!(parent_view.description_identity, child_view.description_identity);
    assert_eq!(parent_view.offset, 31);
    assert_eq!(child_view.offset, 31);
    assert!(!parent_view.flags.closes_on_exec());
    assert!(child_view.flags.closes_on_exec());
    parent.validate().unwrap();
    child.validate().unwrap();
}

#[test]
fn dup2_equal_is() {
    let table = DescriptorTable::new(8).unwrap();
    let source = table
        .install(
            0,
            description(),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();

    assert_eq!(
        table.duplicate_exact(source, source, ExactDuplicate::Dup2).unwrap(),
        source
    );
    assert!(table.lookup(source).unwrap().flags().closes_on_exec());
    assert_eq!(
        table.duplicate_exact(source, source, ExactDuplicate::Dup3(DescriptorFlags::default())),
        Err(DescriptorError::InvalidArgument)
    );
}

#[test]
fn admission_limit_controls_new_descriptors() {
    let table = DescriptorTable::new(8).unwrap();
    let source = table.install(0, description(), DescriptorFlags::default()).unwrap();
    table.set_admission_limit(2);
    assert_eq!(
        table.duplicate(source, 2, DescriptorFlags::default()),
        Err(DescriptorError::InvalidArgument)
    );
    assert_eq!(
        table.duplicate_exact(source, 2, ExactDuplicate::Dup2),
        Err(DescriptorError::BadDescriptor)
    );
    assert_eq!(table.duplicate_exact(source, source, ExactDuplicate::Dup2), Ok(source));
    assert_eq!(table.duplicate(source, 0, DescriptorFlags::default()), Ok(1));
    assert_eq!(
        table.duplicate(source, 0, DescriptorFlags::default()),
        Err(DescriptorError::TooManyOpenFiles)
    );

    table.set_admission_limit(4);
    assert_eq!(table.duplicate(source, 0, DescriptorFlags::default()), Ok(2));
    let child = table.fork();
    assert_eq!(child.duplicate(source, 3, DescriptorFlags::default()), Ok(3));
    assert_eq!(
        child.duplicate(source, 4, DescriptorFlags::default()),
        Err(DescriptorError::InvalidArgument)
    );
}

#[test]
fn exact_duplicate_replaces() {
    let table = DescriptorTable::new(8).unwrap();
    let source = table.install(0, description(), DescriptorFlags::default()).unwrap();
    let replaced = description();
    table
        .install_exact(5, replaced.clone(), DescriptorFlags::default())
        .unwrap();

    table
        .duplicate_exact(
            source,
            5,
            ExactDuplicate::Dup3(DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)),
        )
        .unwrap();

    assert!(!Arc::ptr_eq(table.lookup(5).unwrap().description(), &replaced));
    assert!(table.lookup(5).unwrap().flags().closes_on_exec());
}

#[test]
fn close_on_entries() {
    let table = DescriptorTable::new(8).unwrap();
    let kept = table.install(0, description(), DescriptorFlags::default()).unwrap();
    let removed = table
        .install(
            0,
            description(),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();

    assert_eq!(table.close_on_exec(), vec![removed]);
    assert!(table.lookup(kept).is_ok());
    assert_eq!(table.lookup(removed).unwrap_err(), DescriptorError::BadDescriptor);
}

#[test]
fn fork_preserves_entries() {
    let parent = DescriptorTable::new(8).unwrap();
    let number = parent.install(0, description(), DescriptorFlags::default()).unwrap();
    let child = parent.fork();

    assert!(Arc::ptr_eq(
        parent.lookup(number).unwrap().description(),
        child.lookup(number).unwrap().description()
    ));
    child.close(number).unwrap();
    assert!(parent.lookup(number).is_ok());
}

#[test]
fn invalid_ranges_have() {
    assert_eq!(DescriptorTable::new(-1).unwrap_err(), DescriptorError::InvalidArgument);
    let table = DescriptorTable::new(1).unwrap();
    table.install(0, description(), DescriptorFlags::default()).unwrap();
    assert_eq!(
        table.install(0, description(), DescriptorFlags::default()),
        Err(DescriptorError::TooManyOpenFiles)
    );
    assert_eq!(
        table.duplicate(0, -1, DescriptorFlags::default()),
        Err(DescriptorError::InvalidArgument)
    );
    assert!(matches!(
        table.install_exact(1, description(), DescriptorFlags::default()),
        Err(DescriptorError::BadDescriptor)
    ));
}

#[test]
fn reservation_is_invisible() {
    let table = DescriptorTable::new(8).unwrap();
    let first = table.reserve_exact(3).unwrap();
    assert_eq!(table.lookup(3).unwrap_err(), DescriptorError::BadDescriptor);
    assert_eq!(table.reserve_exact(3).unwrap_err(), DescriptorError::AlreadyExists);
    drop(first);

    let second = table.reserve_exact(3).unwrap();
    table
        .commit(
            second,
            description(),
            StatusFlags::from_bits(0xdead_beef),
            DescriptorFlags::from_bits(0x55),
        )
        .unwrap();
    let snapshot = table.snapshot(3).unwrap();
    assert_eq!(snapshot.descriptor_generation, 2);
    assert_eq!(snapshot.status.bits(), 0xdead_beef);
    assert_eq!(snapshot.flags.bits(), 0x55);
    table.validate().unwrap();
}

#[test]
fn status_and_offset() {
    let table = DescriptorTable::new(8).unwrap();
    let source = table.install(0, description(), DescriptorFlags::default()).unwrap();
    let duplicate = table.duplicate(source, 0, DescriptorFlags::default()).unwrap();

    table.set_status(source, StatusFlags::from_bits(0o0000_4002)).unwrap();
    table.set_offset(duplicate, 91).unwrap();

    let source_snapshot = table.snapshot(source).unwrap();
    let duplicate_snapshot = table.snapshot(duplicate).unwrap();
    assert_eq!(
        source_snapshot.description_identity,
        duplicate_snapshot.description_identity
    );
    assert_eq!(source_snapshot.offset, 91);
    assert_eq!(source_snapshot.status, duplicate_snapshot.status);
    table.validate().unwrap();
}

#[test]
fn final_descriptor_retires() {
    let table = DescriptorTable::new(8).unwrap();
    let lifecycle = Arc::new(LifecycleDescription::default());
    let number = table.install(0, lifecycle.clone(), DescriptorFlags::default()).unwrap();
    let duplicate = table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let held = table.lookup(number).unwrap();

    table.close(number).unwrap();
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 0);
    table.close(duplicate).unwrap();
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 0);
    drop(held);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 1);
}

#[test]
fn fcntl_flag_masks() {
    let descriptor = DescriptorFlags::from_fcntl(u32::MAX);
    assert_eq!(descriptor.bits(), DescriptorFlags::CLOSE_ON_EXEC);

    let original = StatusFlags::from_bits(0o1000_0002 | StatusFlags::APPEND);
    let updated = original.update_from_fcntl(StatusFlags::NONBLOCKING | 0o700);
    assert_eq!(updated.bits(), StatusFlags::PATH_ONLY | 0o2 | StatusFlags::NONBLOCKING);
    let path = StatusFlags::from_bits(StatusFlags::PATH_ONLY | StatusFlags::APPEND);
    assert_eq!(
        path.update_from_fcntl(StatusFlags::NONBLOCKING).bits(),
        StatusFlags::PATH_ONLY | StatusFlags::NONBLOCKING
    );
}

#[test]
fn operation_lease_observes() {
    let table = DescriptorTable::new(8).unwrap();
    let lifecycle = Arc::new(LifecycleDescription::default());
    let number = table.install(0, lifecycle.clone(), DescriptorFlags::default()).unwrap();
    let lease = table.pin(number).unwrap();

    table.close(number).unwrap();

    assert!(lease.retired());
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 0);
    drop(lease);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 1);
}

#[test]
fn transferred_description_preserves() {
    let sender = DescriptorTable::new(8).unwrap();
    let source = sender.install(0, description(), DescriptorFlags::default()).unwrap();
    sender.set_offset(source, 37).unwrap();
    let transferred = sender.export_description(source).unwrap();

    let receiver = DescriptorTable::new(8).unwrap();
    let installed = receiver
        .install_description(
            0,
            &transferred,
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();

    let source_snapshot = sender.snapshot(source).unwrap();
    let received_snapshot = receiver.snapshot(installed).unwrap();
    assert_eq!(
        source_snapshot.description_identity,
        received_snapshot.description_identity
    );
    assert_eq!(received_snapshot.offset, 37);
    assert!(received_snapshot.flags.closes_on_exec());
    sender.validate().unwrap();
    receiver.validate().unwrap();
}

#[test]
fn dropping_table_retires() {
    let lifecycle = Arc::new(LifecycleDescription::default());
    {
        let table = DescriptorTable::new(8).unwrap();
        let source = table.install(0, lifecycle.clone(), DescriptorFlags::default()).unwrap();
        table.duplicate(source, 0, DescriptorFlags::default()).unwrap();
    }
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 1);
}

#[test]
fn forked_tables_share() {
    let parent = DescriptorTable::new(8).unwrap();
    let original = parent.install(0, description(), DescriptorFlags::default()).unwrap();
    let child = parent.fork();
    let parent_new = parent.install(0, description(), DescriptorFlags::default()).unwrap();
    let child_new = child.install(0, description(), DescriptorFlags::default()).unwrap();

    let identities = [
        parent.snapshot(original).unwrap().description_identity,
        parent.snapshot(parent_new).unwrap().description_identity,
        child.snapshot(child_new).unwrap().description_identity,
    ];
    assert_ne!(identities[0], identities[1]);
    assert_ne!(identities[0], identities[2]);
    assert_ne!(identities[1], identities[2]);
    parent.validate().unwrap();
    child.validate().unwrap();
}

fn apply_model_operation(table: &DescriptorTable, model: &mut BTreeMap<i32, (u64, DescriptorFlags)>, random: u64) {
    let operation = (random >> 61) as u8;
    let selected = ((random >> 16) % 32) as i32;
    match operation {
        0 | 1 => {
            let expected = lowest_free_model(model);
            let result = table.install(0, description(), DescriptorFlags::default());
            match (expected, result) {
                (Some(number), Ok(installed)) => {
                    assert_eq!(installed, number);
                    let snapshot = table.snapshot(installed).unwrap();
                    model.insert(installed, (snapshot.description_identity, snapshot.flags));
                }
                (None, Err(DescriptorError::TooManyOpenFiles)) => {}
                pair => panic!("install divergence: {pair:?}"),
            }
        }
        2 | 3 => {
            let result = table.duplicate(selected, 0, DescriptorFlags::default());
            let expected_source = model.get(&selected).copied();
            match (expected_source, result) {
                (None, Err(DescriptorError::BadDescriptor)) => {}
                (Some((identity, _)), Ok(duplicate)) => {
                    let expected = lowest_free_model(model).expect("successful duplicate must have a free slot");
                    assert_eq!(duplicate, expected);
                    model.insert(duplicate, (identity, DescriptorFlags::default()));
                }
                (Some(_), Err(DescriptorError::TooManyOpenFiles)) => {
                    assert_eq!(model.len(), 32);
                }
                pair => panic!("duplicate divergence: {pair:?}"),
            }
        }
        4 => {
            let result = table.close(selected);
            let expected = model.remove(&selected);
            assert_eq!(result.is_ok(), expected.is_some());
        }
        5 => {
            let random_flags = (random >> 8) & u64::from(DescriptorFlags::CLOSE_ON_EXEC);
            let flags = DescriptorFlags::from_bits(u32::try_from(random_flags).expect("masked flags fit in u32"));
            let result = table.set_flags(selected, flags);
            if let Some(entry) = model.get_mut(&selected) {
                entry.1 = flags;
                assert!(result.is_ok());
            } else {
                assert_eq!(result, Err(DescriptorError::BadDescriptor));
            }
        }
        _ => {
            let expected = close_on_exec(model);
            assert_eq!(table.close_on_exec(), expected);
        }
    }
}

#[test]
fn deterministic_operation_trace() {
    let table = DescriptorTable::new(32).unwrap();
    let mut model = BTreeMap::<i32, (u64, DescriptorFlags)>::new();
    let mut random = 0x4d59_5df4_d0f3_3173_u64;

    for _ in 0..10_000 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        apply_model_operation(&table, &mut model, random);
        assert_eq!(table.len(), model.len());
        for (number, (identity, flags)) in &model {
            let snapshot = table.snapshot(*number).unwrap();
            assert_eq!(snapshot.description_identity, *identity);
            assert_eq!(snapshot.flags, *flags);
        }
        table.validate().unwrap();
    }
}
