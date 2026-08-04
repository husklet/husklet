use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use hl_descriptor::{
    DescriptorError, DescriptorFlags, DescriptorTable, ObjectError, ObjectKind, OpenFileDescription, Readiness,
    ReadinessObserver, ReadinessSubscription,
};

use crate::{Control, DescriptorExec, DescriptorImageSlot, PreparedExecParticipant, RuntimeExecError};

#[derive(Debug)]
struct File;

impl OpenFileDescription for File {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
}

#[derive(Debug)]
struct CallbackTarget {
    quiesces: Arc<AtomicUsize>,
}

impl OpenFileDescription for CallbackTarget {
    fn readiness(&self, interests: Readiness) -> Readiness {
        interests
    }

    fn subscribe_readiness(
        &self,
        _: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        Ok(Box::new(CountedSubscription {
            quiesces: self.quiesces.clone(),
            quiesced: AtomicBool::new(false),
        }))
    }
}

struct CountedSubscription {
    quiesces: Arc<AtomicUsize>,
    quiesced: AtomicBool,
}

impl ReadinessSubscription for CountedSubscription {
    fn quiesce(&self) {
        if !self.quiesced.swap(true, Ordering::AcqRel) {
            self.quiesces.fetch_add(1, Ordering::AcqRel);
        }
    }
}

fn fixture() -> (DescriptorImageSlot, i32, i32) {
    let table = DescriptorTable::new(8).unwrap();
    let close = table
        .install(
            0,
            Arc::new(File),
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    let alias = table.duplicate(close, 0, DescriptorFlags::default()).unwrap();
    table.set_offset(close, 17).unwrap();
    (DescriptorImageSlot::new(table), close, alias)
}

#[test]
fn candidate_shared_ofd() {
    let (slot, close, alias) = fixture();
    let (generation, original) = slot.current();
    let original_identity = original.pin(alias).unwrap().description_identity();
    let mut prepared = slot.prepare(generation);

    assert_eq!(original.pin(close).unwrap().offset(), 17);
    prepared.publish().unwrap();

    let (published_generation, published) = slot.current();
    assert_ne!(published_generation, generation);
    assert_eq!(published.pin(close).unwrap_err(), DescriptorError::BadDescriptor);
    let surviving = published.pin(alias).unwrap();
    assert_eq!(surviving.description_identity(), original_identity);
    assert_eq!(surviving.offset(), 17);
    surviving.set_offset(29);
    assert_eq!(original.pin(alias).unwrap().offset(), 29);
    original.validate().unwrap();
    published.validate().unwrap();

    prepared.finish();
}

#[test]
fn candidate_identity() {
    let fixture = EpollExecFixture::new();
    let participant = fixture.participant();
    let mut prepared = participant.prepare_current().unwrap();
    let candidate = prepared.candidate().unwrap();
    prepared.publish().unwrap();
    let (_, current) = fixture.slot.current();
    assert!(Arc::ptr_eq(&candidate, &current));
}

#[test]
fn stale_publish_candidate() {
    let (slot, _, alias) = fixture();
    let (generation, original) = slot.current();
    let mut winner = slot.prepare(generation);
    let mut stale = slot.prepare(generation);
    winner.publish().unwrap();

    assert_eq!(stale.publish(), Err(RuntimeExecError::Failed));
    let (_, published) = slot.current();
    assert!(!Arc::ptr_eq(&published, &original));
    assert!(published.pin(alias).is_ok());
    stale.rollback();
    assert!(Arc::ptr_eq(&slot.current().1, &published));
    winner.finish();
}

#[test]
fn rollback_table_lifetimes() {
    let (slot, close, alias) = fixture();
    let (generation, original) = slot.current();
    let original_alias = original.pin(alias).unwrap();
    let mut prepared = slot.prepare(generation);
    prepared.publish().unwrap();
    let published = slot.current().1;
    let published_alias = published.pin(alias).unwrap();
    assert_eq!(published.pin(close).unwrap_err(), DescriptorError::BadDescriptor,);

    prepared.rollback();

    let (restored_generation, restored) = slot.current();
    assert_eq!(restored_generation, generation);
    assert!(Arc::ptr_eq(&restored, &original));
    assert_eq!(
        restored.pin(alias).unwrap().description_identity(),
        original_alias.description_identity(),
    );
    assert!(restored.pin(close).is_ok());
    drop(published_alias);
    drop(published);
    assert!(!original_alias.retired());
    restored.validate().unwrap();
}

struct EpollExecFixture {
    control: Arc<Control>,
    table: crate::RuntimeDescriptorTable,
    slot: Arc<DescriptorImageSlot>,
}

impl EpollExecFixture {
    fn new() -> Self {
        let (control, table) = Control::new(16, 16).unwrap();
        let slot = Arc::new(DescriptorImageSlot::from_shared(table.descriptor_table().clone()));
        Self {
            control: Arc::new(control),
            table,
            slot,
        }
    }

    fn epoll(&self, close_on_exec: bool) -> i32 {
        let flags = DescriptorFlags::from_bits(if close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        self.control.create_epoll(&self.table, flags).unwrap()
    }

    fn participant(&self) -> DescriptorExec {
        DescriptorExec::new(self.slot.clone(), self.control.clone())
    }

    fn add(&self, source: i32, target: i32) {
        self.control
            .add(
                &self.table,
                source,
                target,
                hl_event::EpollInterest::from_bits(hl_event::EpollInterest::READ),
                7,
            )
            .unwrap();
    }
}

#[test]
fn surviving_identity_live() {
    let fixture = EpollExecFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(true);
    fixture.add(source, target);
    fixture
        .control
        .duplicate(&fixture.table, target, 0, DescriptorFlags::default())
        .unwrap();
    let mut prepared = fixture.participant().prepare_current().unwrap();

    prepared.publish().unwrap();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
    prepared.finish();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);
}

#[test]
fn final_exactly_reversible() {
    let fixture = EpollExecFixture::new();
    let source = fixture.epoll(false);
    let target = fixture.epoll(true);
    fixture.add(source, target);
    let original = fixture.slot.current().1;
    let original_graph = fixture.control.graph_snapshot();
    let mut prepared = fixture.participant().prepare_current().unwrap();

    prepared.publish().unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
    assert!(fixture.slot.current().1.pin(target).is_err());
    prepared.rollback();
    assert_eq!(fixture.control.graph_snapshot(), original_graph);
    assert!(Arc::ptr_eq(&fixture.slot.current().1, &original));
    assert!(fixture.slot.current().1.pin(target).is_ok());
}

#[test]
fn stale_retry_works() {
    let fixture = EpollExecFixture::new();
    let source = fixture.epoll(true);
    let target = fixture.epoll(true);
    fixture.add(source, target);
    let participant = fixture.participant();
    let mut winner = participant.prepare_current().unwrap();
    let mut stale = participant.prepare_current().unwrap();

    winner.publish().unwrap();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
    assert_eq!(stale.publish(), Err(RuntimeExecError::Failed));
    stale.rollback();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
    winner.rollback();
    assert_eq!(fixture.control.graph_snapshot().edges.len(), 1);

    let mut retry = participant.prepare_current().unwrap();
    retry.publish().unwrap();
    retry.finish();
    assert!(fixture.control.graph_snapshot().edges.is_empty());
}

#[test]
fn final_until_finish() {
    let (control, table) = Control::new(8, 8).unwrap();
    let control = Arc::new(control);
    let quiesces = Arc::new(AtomicUsize::new(0));
    let target = table
        .descriptor_table()
        .install(
            0,
            Arc::new(CallbackTarget {
                quiesces: quiesces.clone(),
            }),
            DescriptorFlags::default(),
        )
        .unwrap();
    let source = control
        .create_epoll(&table, DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC))
        .unwrap();
    control
        .add(
            &table,
            source,
            target,
            hl_event::EpollInterest::from_bits(hl_event::EpollInterest::READ),
            9,
        )
        .unwrap();
    let slot = Arc::new(DescriptorImageSlot::from_shared(table.descriptor_table().clone()));
    let participant = DescriptorExec::new(slot, control);
    let mut prepared = participant.prepare_current().unwrap();
    drop(table);

    prepared.publish().unwrap();
    assert_eq!(quiesces.load(Ordering::Acquire), 0);
    prepared.finish();
    assert_eq!(quiesces.load(Ordering::Acquire), 1);
}
