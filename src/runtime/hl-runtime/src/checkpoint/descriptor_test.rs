use std::sync::Arc;

use hl_checkpoint::{
    CheckpointImage, CheckpointReader, CheckpointWriter, ImageLimits, MemorySink, MemorySource, Section, SectionKind,
};
use hl_descriptor::{
    DescriptorCheckpointError, DescriptorFlags, DescriptorObjectCheckpoint, DescriptorTable, ObjectKind,
    OpenDescriptionImage, OpenFileDescription, StatusFlags,
};

use crate::{
    CheckpointDescriptorTable, CheckpointParticipant, DescriptorCheckpointParticipant, DescriptorObjectCatalog,
    DirectoryObjectCatalog, DirectoryObjectCheckpoint,
};
use std::time::Instant;

#[derive(Debug)]
struct File;

impl OpenFileDescription for File {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
}

struct Objects;

#[derive(Debug)]
struct Directory(std::sync::Mutex<u8>);

impl OpenFileDescription for Directory {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Directory
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, hl_descriptor::ObjectError> {
        let mut cursor = self.0.lock().unwrap();
        if let Some(first) = output.first_mut() {
            *first = *cursor;
            *cursor += 1;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

struct Directories;

impl DescriptorObjectCheckpoint for Directories {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(1)
    }
    fn snapshot_into(
        &self,
        _: u64,
        object: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        let directory = object
            .domain_extension()
            .and_then(|value| value.downcast_ref::<Directory>())
            .ok_or(DescriptorCheckpointError::Object)?;
        output[0] = *directory.0.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        Ok(())
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        Ok(Arc::new(Directory(std::sync::Mutex::new(
            *description.object.first().ok_or(DescriptorCheckpointError::Object)?,
        ))))
    }
}

impl DirectoryObjectCheckpoint for Directories {
    fn payload_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(1)
    }

    fn owns(&self, _: u64, object: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
        Ok(object
            .domain_extension()
            .is_some_and(<dyn std::any::Any>::is::<Directory>))
    }
}

impl DescriptorObjectCheckpoint for Objects {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(1)
    }
    fn snapshot_into(
        &self,
        identity: u64,
        _: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        output[0] = identity as u8;
        Ok(())
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

fn checkpoint_image(bytes: Vec<u8>) -> CheckpointImage {
    let mut writer = CheckpointWriter::new(ImageLimits::default());
    writer
        .push(Section::new(SectionKind::new(2).unwrap(), 1, bytes))
        .unwrap();
    let mut sink = MemorySink::new();
    writer.publish(&mut sink).unwrap();
    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    CheckpointReader::new(ImageLimits::default()).read(&mut source).unwrap()
}

fn checkpoint_fixture(count: usize) -> (DescriptorCheckpointParticipant, CheckpointImage) {
    let table = Arc::new(DescriptorTable::new(count as i32).unwrap());
    for _ in 0..count {
        table.install(0, Arc::new(File), DescriptorFlags::default()).unwrap();
    }
    let handle = Arc::new(CheckpointDescriptorTable::new(table));
    let participant = DescriptorCheckpointParticipant::new(
        handle,
        Arc::new(DescriptorObjectCatalog::rejecting().bind(ObjectKind::File, Arc::new(Objects))),
    );
    participant.freeze().unwrap();
    let bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    (participant, checkpoint_image(bytes))
}

#[test]
#[ignore = "performance diagnostic"]
fn validated_stage_benchmark() {
    for (count, rounds) in [(2, 200), (4_096, 8)] {
        let (participant, image) = checkpoint_fixture(count);
        let section = image.section(SectionKind::new(2).unwrap()).unwrap();
        let started = Instant::now();
        for _ in 0..rounds {
            participant.validate(&image, section).unwrap();
            let reservation = participant.stage_bound(image.digest(), section).unwrap();
            participant.rollback(reservation);
        }
        println!(
            "descriptor_checkpoint_count={count} ns={}",
            started.elapsed().as_nanos() / rounds as u128
        );
    }
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
fn alias_cursor_shared() {
    let table = DescriptorTable::new(4).unwrap();
    let first = table
        .install(
            0,
            Arc::new(Directory(std::sync::Mutex::new(7))),
            DescriptorFlags::default(),
        )
        .unwrap();
    let alias = table.duplicate(first, 0, DescriptorFlags::default()).unwrap();
    let directories = Arc::new(DirectoryObjectCatalog::rejecting().bind(1, Arc::new(Directories)));
    let catalog = DescriptorObjectCatalog::rejecting().bind(ObjectKind::Directory, directories);

    table.freeze_checkpoint();
    let image = table.checkpoint_image(&catalog).unwrap();
    table.thaw_checkpoint();
    let restored = DescriptorTable::restore_checkpoint(&image, &catalog).unwrap();

    let mut byte = [0];
    assert_eq!(restored.pin(first).unwrap().read(&mut byte).unwrap(), 1);
    assert_eq!(byte, [7]);
    assert_eq!(restored.pin(alias).unwrap().read(&mut byte).unwrap(), 1);
    assert_eq!(byte, [8]);
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
