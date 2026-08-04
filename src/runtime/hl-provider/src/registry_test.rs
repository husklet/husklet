use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::epoll_registry::{EPOLL_EDGE_TRIGGERED, EPOLL_ONE_SHOT};
use crate::{EpollRegistry, RegistryError, WatchConfig, WatchIdentity};

struct EpollFixture;

impl EpollFixture {
    fn identity(descriptor: i32) -> WatchIdentity {
        WatchIdentity {
            epoll: 3,
            epoll_generation: 1,
            descriptor,
            descriptor_generation: 1,
        }
    }

    fn config(events: u32, interests: u32, data: u64) -> WatchConfig {
        WatchConfig {
            remote_handle: 9,
            events,
            interests,
            data,
        }
    }
}

#[test]
fn lifecycle_capacity_and() {
    let registry = EpollRegistry::new(1).unwrap();
    let token = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 3, 7))
        .unwrap();
    assert_eq!(registry.find(EpollFixture::identity(4)), None);
    assert_eq!(
        registry.reserve(EpollFixture::identity(5), EpollFixture::config(0, 3, 8)),
        Err(RegistryError::Capacity)
    );
    registry.activate(token).unwrap();
    assert_eq!(registry.find(EpollFixture::identity(4)), Some(token));
    assert_eq!(
        registry.reserve(EpollFixture::identity(4), EpollFixture::config(0, 3, 8)),
        Err(RegistryError::Duplicate)
    );
    registry.retire(token).unwrap();
    let next = registry
        .reserve(EpollFixture::identity(5), EpollFixture::config(0, 3, 8))
        .unwrap();
    assert_ne!(token.generation(), next.generation());
    assert_eq!(registry.activate(token), Err(RegistryError::InvalidToken));
    registry.cancel(next).unwrap();
}

#[test]
fn replace_supports_same() {
    let registry = EpollRegistry::new(2).unwrap();
    let old = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 1, 7))
        .unwrap();
    registry.activate(old).unwrap();
    let new = registry
        .replace(
            old,
            EpollFixture::identity(4),
            EpollFixture::config(EPOLL_EDGE_TRIGGERED, 2, 8),
        )
        .unwrap();
    assert_eq!(registry.find(EpollFixture::identity(4)), Some(new));
    assert!(registry.callback(old).is_none());
    assert_eq!(registry.snapshot().watches[0].config.data, 8);
}

#[test]
fn full_capacity_replace() {
    let registry = EpollRegistry::new(1).unwrap();
    let old = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 1, 7))
        .unwrap();
    registry.activate(old).unwrap();

    let new = registry
        .replace(
            old,
            EpollFixture::identity(4),
            EpollFixture::config(EPOLL_EDGE_TRIGGERED, 2, 8),
        )
        .unwrap();

    assert_eq!(registry.find(EpollFixture::identity(4)), Some(new));
    assert_ne!(old, new);
    assert!(registry.callback(old).is_none());
    assert_eq!(registry.snapshot().watches[0].config.data, 8);
}

#[test]
fn level_edge_and() {
    let registry = EpollRegistry::new(3).unwrap();
    let level = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 3, 1))
        .unwrap();
    let edge = registry
        .reserve(
            EpollFixture::identity(5),
            EpollFixture::config(EPOLL_EDGE_TRIGGERED, 3, 2),
        )
        .unwrap();
    let one = registry
        .reserve(EpollFixture::identity(6), EpollFixture::config(EPOLL_ONE_SHOT, 3, 3))
        .unwrap();
    for token in [level, edge, one] {
        registry.activate(token).unwrap();
        registry.callback(token).unwrap().ready(3);
    }
    assert_eq!(registry.take_ready(level, 1).unwrap().unwrap().readiness, 3);
    assert_eq!(registry.take_ready(level, 0).unwrap().unwrap().readiness, 1);
    assert_eq!(registry.take_ready(edge, 1).unwrap().unwrap().readiness, 3);
    assert_eq!(registry.take_ready(edge, 1).unwrap(), None);
    assert!(registry.take_ready(one, 0).unwrap().unwrap().unsubscribe);
    registry.callback(one).unwrap().ready(3);
    assert_eq!(registry.take_ready(one, 0).unwrap(), None);
    assert_eq!(EpollRegistry::linux_events(0b1_1111), 0x01f);
}

#[test]
fn retire_waits_for() {
    let registry = EpollRegistry::new(1).unwrap();
    let token = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 1, 1))
        .unwrap();
    registry.activate(token).unwrap();
    let lease = registry.callback(token).unwrap();
    let retired = registry.clone();
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || {
        retired.retire(token).unwrap();
        tx.send(()).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
    lease.ready(1);
    drop(lease);
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    join.join().unwrap();
    assert!(registry.callback(token).is_none());
}

#[test]
fn reset_returns_reconnect() {
    let registry = EpollRegistry::new(2).unwrap();
    let old = registry
        .reserve(EpollFixture::identity(4), EpollFixture::config(0, 1, 1))
        .unwrap();
    registry.activate(old).unwrap();
    registry.callback(old).unwrap().ready(1);
    let snapshot = registry.reset();
    assert_eq!(snapshot.capacity, 2);
    assert_eq!(snapshot.watches.len(), 1);
    assert_eq!(snapshot.watches[0].ready, 1);
    assert!(registry.snapshot().watches.is_empty());
    assert!(registry.callback(old).is_none());
    for descriptor in 10..1010 {
        let token = registry
            .reserve(
                EpollFixture::identity(descriptor),
                EpollFixture::config(0, 1, descriptor as u64),
            )
            .unwrap();
        registry.activate(token).unwrap();
        registry.retire(token).unwrap();
    }
    assert!(registry.callback(old).is_none());
}
