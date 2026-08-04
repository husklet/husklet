use std::sync::{Arc, Mutex};
use std::thread;

use crate::{
    DESCRIPTOR_CHECKPOINT_VERSION, DescriptorCheckpointError, DescriptorFlags, DescriptorObjectCheckpoint,
    DescriptorTable, DescriptorTableImage, ObjectError, ObjectKind, OpenDescriptionImage, OpenFileDescription,
    OperationActor, OperationCancellation, OperationContext, StatusFlags,
};

#[derive(Debug)]
struct Object {
    value: u8,
}

impl OpenFileDescription for Object {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        if let Some(first) = output.first_mut() {
            *first = self.value;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        Ok(input.len())
    }
}

struct Cancellation;
struct Subscription;

impl crate::CancellationSubscription for Subscription {}

impl OperationCancellation for Cancellation {
    fn interrupted(&self) -> bool {
        false
    }
    fn subscribe(&self, _: Arc<dyn crate::CancellationNotification>) -> Box<dyn crate::CancellationSubscription> {
        Box::new(Subscription)
    }
}

#[derive(Default)]
struct Objects {
    rebound: Mutex<Vec<u64>>,
}

impl DescriptorObjectCheckpoint for Objects {
    fn snapshot(&self, identity: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        Ok(vec![identity as u8])
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        self.rebound.lock().unwrap().push(description.identity);
        Ok(Arc::new(Object {
            value: *description.object.first().ok_or(DescriptorCheckpointError::Object)?,
        }))
    }
}

fn populated() -> (DescriptorTable, Arc<Objects>, i32, i32) {
    let table = DescriptorTable::new(32).unwrap();
    let first = table
        .install(
            0,
            Arc::new(Object { value: 17 }),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    table.set_offset(first, 91).unwrap();
    table
        .set_status(first, StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let alias = table.duplicate(first, 0, DescriptorFlags::default()).unwrap();
    (table, Arc::new(Objects::default()), first, alias)
}

#[test]
fn aggregate_round_trip() {
    let (table, objects, first, alias) = populated();
    table.freeze_checkpoint();
    let image = table.checkpoint_image(objects.as_ref()).unwrap();
    table.thaw_checkpoint();
    assert_eq!(image.descriptions.len(), 1);
    assert_eq!(image.entries.len(), 2);

    let restored = DescriptorTable::restore_checkpoint(&image, objects.as_ref()).unwrap();
    assert_eq!(restored.snapshot(first).unwrap(), table.snapshot(first).unwrap());
    assert_eq!(restored.snapshot(alias).unwrap(), table.snapshot(alias).unwrap());
    restored.set_offset(alias, 777).unwrap();
    assert_eq!(restored.snapshot(first).unwrap().offset, 777);
    assert!(restored.snapshot(first).unwrap().flags.closes_on_exec());
    assert!(!restored.snapshot(alias).unwrap().flags.closes_on_exec());
    assert_eq!(*objects.rebound.lock().unwrap(), vec![image.descriptions[0].identity]);
}

#[test]
fn duplicate_stale_and() {
    let (table, objects, _, _) = populated();
    table.freeze_checkpoint();
    let image = table.checkpoint_image(objects.as_ref()).unwrap();
    table.thaw_checkpoint();
    for corrupt in 0..3 {
        let mut invalid = image.clone();
        let expected = match corrupt {
            0 => {
                invalid.entries[1].number = invalid.entries[0].number;
                DescriptorCheckpointError::DuplicateNumber
            }
            1 => {
                invalid.entries[0].generation = invalid.entries[0].generation.saturating_add(1);
                DescriptorCheckpointError::StaleGeneration
            }
            _ => {
                invalid.entries[0].description_identity = u64::MAX;
                DescriptorCheckpointError::MissingDescription
            }
        };
        assert!(matches!(
            DescriptorTable::restore_checkpoint(&invalid, objects.as_ref()),
            Err(error) if error == expected
        ));
    }
    assert!(objects.rebound.lock().unwrap().is_empty());
}

#[test]
fn freeze_waits_for() {
    let (table, _, first, _) = populated();
    let table = Arc::new(table);
    let lease = table.pin(first).unwrap();
    let frozen = table.clone();
    let waiter = thread::spawn(move || {
        frozen.freeze_checkpoint();
        frozen
    });
    thread::yield_now();
    assert!(!waiter.is_finished());
    drop(lease);
    let frozen = waiter.join().unwrap();
    assert_eq!(
        frozen.set_flags(first, DescriptorFlags::default()),
        Err(crate::DescriptorError::CheckpointFrozen)
    );
    frozen.thaw_checkpoint();
    frozen.set_flags(first, DescriptorFlags::default()).unwrap();
}

#[test]
fn durable_pin_quiescence() {
    let (table, _, first, _) = populated();
    table.freeze_checkpoint();
    let lease = table.pin_checkpoint(first).unwrap();
    table.thaw_checkpoint();
    table.freeze_checkpoint();
    assert_eq!(lease.descriptor_number(), first);
    table.thaw_checkpoint();
    drop(lease);
    assert_eq!(
        table.pin_checkpoint(first).unwrap_err(),
        crate::DescriptorError::Corrupt,
    );
}

#[test]
fn context_io_defaults() {
    let table = DescriptorTable::new(4).unwrap();
    let descriptor = table
        .install(0, Arc::new(Object { value: 29 }), DescriptorFlags::default())
        .unwrap();
    let lease = table.pin(descriptor).unwrap();
    let cancellation = Cancellation;
    let context = OperationContext {
        actor: Some(OperationActor {
            process: 7,
            process_generation: 1,
            thread: 9,
            thread_generation: 1,
        }),
        cancellation: Some(&cancellation),
    };
    let mut output = [0_u8; 1];
    assert_eq!(lease.read_context(&mut output, context), Ok(1));
    assert_eq!(output, [29]);
    assert_eq!(lease.write_context(&[1, 2, 3], context), Ok(3));
}

#[test]
fn transfer_root_survives() {
    let table = DescriptorTable::new(8).unwrap();
    let objects = Objects::default();
    let number = table
        .install(0, Arc::new(Object { value: 17 }), DescriptorFlags::default())
        .unwrap();
    let queued = table.export_description(number).unwrap();
    let identity = queued.identity();
    table.close(number).unwrap();
    table.freeze_checkpoint();
    let image = table.checkpoint_image(&objects).unwrap();
    table.thaw_checkpoint();
    assert!(image.entries.is_empty());
    assert_eq!(image.descriptions.len(), 1);

    let restored = DescriptorTable::restore_checkpoint(&image, &objects).unwrap();
    restored.freeze_checkpoint();
    let rebound = restored.export_checkpoint_identity(identity).unwrap();
    restored.release_checkpoint_roots();
    restored.thaw_checkpoint();
    assert_eq!(rebound.identity(), identity);
}

#[test]
fn transfer_root_bound() {
    let mut image = DescriptorTableImage {
        version: DESCRIPTOR_CHECKPOINT_VERSION,
        limit: 1,
        generations: Vec::new(),
        descriptions: Vec::new(),
        entries: Vec::new(),
    };
    for identity in 1..=2 {
        image.descriptions.push(OpenDescriptionImage {
            identity,
            generation: 1,
            offset: 0,
            status: StatusFlags::default(),
            kind: ObjectKind::File,
            object: Vec::new(),
        });
    }
    assert_eq!(image.validate(), Err(DescriptorCheckpointError::Limit));
}
