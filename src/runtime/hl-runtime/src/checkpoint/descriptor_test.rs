use std::sync::Arc;

use hl_checkpoint::{Section, SectionKind};
use hl_descriptor::{
    DescriptorCheckpointError, DescriptorFlags, DescriptorObjectCheckpoint, DescriptorTable, ObjectKind,
    OpenDescriptionImage, OpenFileDescription, StatusFlags,
};

use crate::{
    CheckpointDescriptorTable, CheckpointParticipant, DescriptorCheckpointParticipant, DescriptorObjectCatalog,
};

#[derive(Debug)]
struct File;

impl OpenFileDescription for File {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
}

struct Objects;

impl DescriptorObjectCheckpoint for Objects {
    fn snapshot(&self, identity: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        Ok(vec![identity as u8])
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        description.object.first().ok_or(DescriptorCheckpointError::Object)?;
        Ok(Arc::new(File))
    }
}

fn fixture() -> (
    Arc<CheckpointDescriptorTable>,
    DescriptorCheckpointParticipant,
    i32,
    i32,
) {
    let table = Arc::new(DescriptorTable::new(16).unwrap());
    let first = table
        .install(
            0,
            Arc::new(File),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    let alias = table.duplicate(first, 0, DescriptorFlags::default()).unwrap();
    table.set_offset(first, 41).unwrap();
    table
        .set_status(first, StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let handle = Arc::new(CheckpointDescriptorTable::new(table));
    let participant = DescriptorCheckpointParticipant::new(
        handle.clone(),
        Arc::new(DescriptorObjectCatalog::rejecting().bind(ObjectKind::File, Arc::new(Objects))),
    );
    (handle, participant, first, alias)
}

#[test]
fn unknown_object_rejected() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    table.install(0, Arc::new(File), DescriptorFlags::default()).unwrap();
    let handle = Arc::new(CheckpointDescriptorTable::new(table));
    let participant = DescriptorCheckpointParticipant::new(handle, Arc::new(DescriptorObjectCatalog::rejecting()));
    participant.freeze().unwrap();
    assert!(participant.snapshot().is_err());
    participant.thaw().unwrap();
}

#[test]
fn participant_independent_table() {
    let (handle, participant, first, alias) = fixture();
    let original = handle.current();
    participant.freeze().unwrap();
    let bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    let section = Section::new(SectionKind::new(2).unwrap(), participant.version(), bytes);
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    let restored = handle.current();
    assert!(!Arc::ptr_eq(&original, &restored));
    participant.resume(reservation).unwrap();
    assert_eq!(restored.snapshot(first).unwrap().offset, 41);
    restored.set_offset(alias, 88).unwrap();
    assert_eq!(restored.snapshot(first).unwrap().offset, 88);
    assert!(restored.snapshot(first).unwrap().flags.closes_on_exec());
    assert!(!restored.snapshot(alias).unwrap().flags.closes_on_exec());
}

#[test]
fn rollback_previous_table() {
    let (handle, participant, _, _) = fixture();
    let original = handle.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(2).unwrap(),
        participant.version(),
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &original));
    assert!(original.install(0, Arc::new(File), DescriptorFlags::default(),).is_ok());
}

#[test]
fn malformed_unchanged_running() {
    let (handle, participant, _, _) = fixture();
    let original = handle.current();
    let section = Section::new(SectionKind::new(2).unwrap(), participant.version(), vec![1, 2, 3]);
    assert!(participant.stage(&section).is_err());
    assert!(Arc::ptr_eq(&handle.current(), &original));
    assert!(original.install(0, Arc::new(File), DescriptorFlags::default(),).is_ok());
}
