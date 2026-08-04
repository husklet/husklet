use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hl_descriptor::{DescriptorFlags, DescriptorTable, OpenFileDescription, Readiness, StatusFlags};

use crate::{Epoll, EpollError, EpollInterest, EventFd, EventFdFlags};

struct Fixture {
    table: DescriptorTable,
}

impl Fixture {
    fn new() -> Self {
        Self {
            table: DescriptorTable::new(32).unwrap(),
        }
    }

    fn eventfd(&self) -> (i32, Arc<EventFd>) {
        let object = Arc::new(EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap());
        let number = self
            .table
            .commit(
                self.table.reserve(0).unwrap(),
                object.clone(),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        (number, object)
    }

    fn epoll(&self) -> (i32, Arc<Epoll>) {
        let object = Arc::new(Epoll::new());
        let number = self
            .table
            .commit(
                self.table.reserve(0).unwrap(),
                object.clone(),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        (number, object)
    }

    fn read_interest() -> EpollInterest {
        EpollInterest::from_bits(EpollInterest::READ)
    }

    fn write_one(eventfd: &EventFd) {
        eventfd.write(&1_u64.to_ne_bytes()).unwrap();
    }

    fn drain(eventfd: &EventFd) {
        let mut output = [0_u8; 8];
        eventfd.read(&mut output).unwrap();
    }
}

#[test]
fn add_modify_shape() {
    let fixture = Fixture::new();
    let (number, _) = fixture.eventfd();
    let epoll = Epoll::new();
    let lease = fixture.table.pin(number).unwrap();
    let key = epoll.add(lease.clone(), Fixture::read_interest(), 11).unwrap();
    assert_eq!(key.descriptor_number, number);
    assert_eq!(
        epoll.add(lease.clone(), Fixture::read_interest(), 12),
        Err(EpollError::AlreadyExists)
    );
    epoll.modify(&lease, Fixture::read_interest(), 13).unwrap();
    epoll.delete(&lease).unwrap();
    assert_eq!(epoll.delete(&lease), Err(EpollError::NotFound));
    assert_eq!(
        epoll.modify(&lease, Fixture::read_interest(), 0),
        Err(EpollError::NotFound)
    );
    assert_eq!(epoll.sample(0), Err(EpollError::InvalidArgument));
}

#[test]
fn level_trigger_drained() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    epoll
        .add(fixture.table.pin(number).unwrap(), Fixture::read_interest(), 21)
        .unwrap();
    Fixture::write_one(&eventfd);
    for _ in 0..2 {
        let events = epoll.sample(4).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, 21);
        assert!(events[0].readiness.contains(Readiness::READ));
    }
    Fixture::drain(&eventfd);
    assert!(epoll.sample(4).unwrap().is_empty());
}

#[test]
fn edge_trigger_transitions() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    let interests = EpollInterest::from_bits(EpollInterest::READ | EpollInterest::EDGE_TRIGGERED);
    epoll.add(fixture.table.pin(number).unwrap(), interests, 31).unwrap();
    Fixture::write_one(&eventfd);
    assert_eq!(epoll.sample(4).unwrap().len(), 1);
    assert!(epoll.sample(4).unwrap().is_empty());
    Fixture::drain(&eventfd);
    Fixture::write_one(&eventfd);
    assert_eq!(epoll.sample(4).unwrap().len(), 1);
}

#[test]
fn abandoned_peek_oneshot() {
    for mode in [EpollInterest::EDGE_TRIGGERED, EpollInterest::ONESHOT] {
        let fixture = Fixture::new();
        let (number, eventfd) = fixture.eventfd();
        let epoll = Epoll::new();
        epoll
            .add(
                fixture.table.pin(number).unwrap(),
                EpollInterest::from_bits(EpollInterest::READ | mode),
                35,
            )
            .unwrap();
        Fixture::write_one(&eventfd);
        assert_eq!(epoll.peek(1).unwrap().events().len(), 1);
        let retry = epoll.peek(1).unwrap();
        assert_eq!(retry.events()[0].data, 35);
        assert!(epoll.commit(retry).unwrap());
        assert!(epoll.sample(1).unwrap().is_empty());
    }
}

#[test]
fn control_change_replacement() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    let lease = fixture.table.pin(number).unwrap();
    let interest = Fixture::read_interest();
    epoll.add(lease.clone(), interest, 36).unwrap();
    Fixture::write_one(&eventfd);
    let stale = epoll.peek(1).unwrap();
    epoll.modify(&lease, interest, 37).unwrap();
    assert!(!epoll.commit(stale).unwrap());
    assert_eq!(epoll.sample(1).unwrap()[0].data, 37);

    let stale = epoll.peek(1).unwrap();
    epoll.delete(&lease).unwrap();
    assert!(!epoll.commit(stale).unwrap());
}

#[test]
fn new_edge_commit() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    epoll
        .add(
            fixture.table.pin(number).unwrap(),
            EpollInterest::from_bits(EpollInterest::READ | EpollInterest::EDGE_TRIGGERED),
            38,
        )
        .unwrap();
    Fixture::write_one(&eventfd);
    let first = epoll.peek(1).unwrap();
    Fixture::drain(&eventfd);
    Fixture::write_one(&eventfd);
    assert!(epoll.commit(first).unwrap());
    assert_eq!(epoll.sample(1).unwrap()[0].data, 38);
}

#[test]
fn oneshot_disarms_change() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    let lease = fixture.table.pin(number).unwrap();
    let interests = EpollInterest::from_bits(EpollInterest::READ | EpollInterest::ONESHOT);
    epoll.add(lease.clone(), interests, 41).unwrap();
    Fixture::write_one(&eventfd);
    assert_eq!(epoll.sample(4).unwrap()[0].data, 41);
    assert!(epoll.sample(4).unwrap().is_empty());
    epoll.modify(&lease, interests, 42).unwrap();
    assert_eq!(epoll.sample(4).unwrap()[0].data, 42);
}

#[test]
fn distinct_aliases_description() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let alias = fixture.table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let epoll = Epoll::new();
    let first = fixture.table.pin(number).unwrap();
    let second = fixture.table.pin(alias).unwrap();
    assert_eq!(first.description_identity(), second.description_identity());
    epoll.add(first, Fixture::read_interest(), 51).unwrap();
    epoll.add(second, Fixture::read_interest(), 52).unwrap();
    Fixture::write_one(&eventfd);
    let events = epoll.sample(4).unwrap();
    assert_eq!(events.iter().map(|event| event.data).collect::<Vec<_>>(), vec![51, 52]);
}

#[test]
fn final_close_it() {
    let fixture = Fixture::new();
    let (number, original) = fixture.eventfd();
    let alias = fixture.table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let epoll = Epoll::new();
    epoll
        .add(fixture.table.pin(number).unwrap(), Fixture::read_interest(), 61)
        .unwrap();
    fixture.table.close(number).unwrap();
    Fixture::write_one(&original);
    assert_eq!(epoll.sample(4).unwrap()[0].data, 61);
    Fixture::drain(&original);
    fixture.table.close(alias).unwrap();

    let (reused, replacement) = fixture.eventfd();
    assert_eq!(reused, number);
    epoll
        .add(fixture.table.pin(reused).unwrap(), Fixture::read_interest(), 62)
        .unwrap();
    Fixture::write_one(&replacement);
    let events = epoll.sample(4).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, 62);
}

#[test]
fn max_events_watch() {
    let fixture = Fixture::new();
    let (first_number, first) = fixture.eventfd();
    let (second_number, second) = fixture.eventfd();
    let epoll = Epoll::new();
    epoll
        .add(fixture.table.pin(first_number).unwrap(), Fixture::read_interest(), 71)
        .unwrap();
    epoll
        .add(fixture.table.pin(second_number).unwrap(), Fixture::read_interest(), 72)
        .unwrap();
    Fixture::write_one(&first);
    Fixture::write_one(&second);
    assert_eq!(epoll.sample(1).unwrap()[0].data, 71);
    assert_eq!(epoll.sample(1).unwrap()[0].data, 72);
    assert_eq!(epoll.sample(1).unwrap()[0].data, 71);
}

#[test]
fn alias_final_close() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let alias = fixture.table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let epoll = Epoll::new();
    epoll
        .add(fixture.table.pin(number).unwrap(), Fixture::read_interest(), 73)
        .unwrap();
    fixture.table.close(number).unwrap();
    Fixture::write_one(&eventfd);
    assert_eq!(epoll.sample(1).unwrap()[0].data, 73);
    fixture.table.close(alias).unwrap();
    assert!(epoll.sample(1).unwrap().is_empty());
}

#[test]
fn transfer_retains_ready_target() {
    let sender = DescriptorTable::new(8).unwrap();
    let target = Arc::new(EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap());
    let target_number = sender.install(0, target.clone(), DescriptorFlags::default()).unwrap();
    let epoll = Arc::new(Epoll::new());
    let epoll_number = sender.install(0, epoll.clone(), DescriptorFlags::default()).unwrap();
    epoll
        .add(sender.pin(target_number).unwrap(), Fixture::read_interest(), 74)
        .unwrap();

    let queued = sender.export_description(epoll_number).unwrap();
    let receiver = DescriptorTable::new(8).unwrap();
    let received = receiver
        .install_description(0, &queued, DescriptorFlags::default())
        .unwrap();
    drop(queued);

    Fixture::write_one(&target);
    sender.close(target_number).unwrap();
    assert_eq!(epoll.sample(1).unwrap()[0].data, 74);

    receiver.close(received).unwrap();
    assert!(epoll.sample(1).unwrap().is_empty());
}

#[test]
fn final_close_discards_queued_target() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Epoll::new();
    epoll
        .add(fixture.table.pin(number).unwrap(), Fixture::read_interest(), 75)
        .unwrap();
    Fixture::write_one(&eventfd);
    fixture.table.close(number).unwrap();
    assert!(epoll.sample(1).unwrap().is_empty());
}

#[test]
fn blocking_wait_retirement() {
    let fixture = Fixture::new();
    let (number, eventfd) = fixture.eventfd();
    let epoll = Arc::new(Epoll::new());
    epoll
        .add(fixture.table.pin(number).unwrap(), Fixture::read_interest(), 81)
        .unwrap();
    let waiter = epoll.clone();
    let thread = thread::spawn(move || waiter.wait(1, None));
    thread::sleep(Duration::from_millis(10));
    Fixture::write_one(&eventfd);
    assert_eq!(thread.join().unwrap().unwrap()[0].data, 81);
    Fixture::drain(&eventfd);
    assert!(epoll.wait(1, Some(Duration::from_millis(1))).unwrap().is_empty());

    let waiter = epoll.clone();
    let thread = thread::spawn(move || waiter.wait(1, None));
    thread::sleep(Duration::from_millis(10));
    OpenFileDescription::retire(epoll.as_ref());
    assert_eq!(thread.join().unwrap(), Err(EpollError::Retired));
}

#[test]
fn capacity_snapshot_explicit() {
    let fixture = Fixture::new();
    let (first_number, _) = fixture.eventfd();
    let (second_number, _) = fixture.eventfd();
    let epoll = Epoll::with_watch_limit(1).unwrap();
    epoll
        .add(fixture.table.pin(first_number).unwrap(), Fixture::read_interest(), 91)
        .unwrap();
    assert_eq!(
        epoll.add(fixture.table.pin(second_number).unwrap(), Fixture::read_interest(), 92,),
        Err(EpollError::ResourceLimit)
    );
    let snapshot = epoll.snapshot();
    assert_eq!(snapshot.watch_limit, 1);
    assert_eq!(snapshot.watches.len(), 1);
    assert_eq!(snapshot.watches[0].data, 91);
}

#[test]
fn nested_epoll_rejection() {
    let fixture = Fixture::new();
    let (nested_number, nested) = fixture.epoll();
    let outer = Epoll::new();
    outer
        .add(fixture.table.pin(nested_number).unwrap(), Fixture::read_interest(), 101)
        .unwrap();
    let (target_number, target) = fixture.eventfd();
    nested
        .add(fixture.table.pin(target_number).unwrap(), Fixture::read_interest(), 102)
        .unwrap();
    Fixture::write_one(&target);
    let events = outer.sample(1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, 101);
}
