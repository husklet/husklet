use super::*;

use crate::MemoryPort;
use crate::test_support::ProcessFixture;

#[test]
fn restores_everything() {
    let fixture = ProcessFixture::new();
    let shared = fixture.ipc.shared.snapshot();
    let semaphores = fixture.ipc.semaphores.snapshot();
    let bindings = fixture.mappings.bindings().unwrap();
    let participant = ExitHandler::new(fixture.ipc.catalog.clone(), fixture.mappings.clone(), Arc::new(|| 12));
    let mut prepared = participant.prepare(fixture.process, &[fixture.thread]).unwrap();
    prepared.publish().unwrap();
    let applied = fixture.ipc.semaphores.snapshot();
    assert!(applied.undo.is_empty());
    assert_eq!(applied.sets[0].values, vec![0, 0]);
    assert!(fixture.ipc.shared.snapshot().attachments.is_empty());
    prepared.rollback();
    assert_eq!(fixture.ipc.shared.snapshot(), shared);
    assert_eq!(fixture.ipc.semaphores.snapshot(), semaphores);
    assert_eq!(fixture.mappings.bindings().unwrap(), bindings);
}

#[test]
fn undo_twice() {
    let fixture = ProcessFixture::new();
    let participant = ExitHandler::new(fixture.ipc.catalog.clone(), fixture.mappings.clone(), Arc::new(|| 13));
    let mut first = participant.prepare(fixture.process, &[fixture.thread]).unwrap();
    first.publish().unwrap();
    first.rollback();

    let mut retry = participant.prepare(fixture.process, &[fixture.thread]).unwrap();
    retry.publish().unwrap();
    retry.finish();
    let snapshot = fixture.ipc.semaphores.snapshot();
    assert!(snapshot.undo.is_empty());
    assert_eq!(snapshot.sets[0].values, vec![0, 0]);
}
