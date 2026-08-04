use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use super::*;
use crate::test_support::Endpoint;

struct Collector {
    events: mpsc::Sender<ProviderEvent>,
}

impl EventObserver for Collector {
    fn provider_event(&self, event: ProviderEvent) {
        self.events.send(event).unwrap();
    }
}

struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut released = self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*released {
            released = self
                .changed
                .wait(released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

struct BlockingCollector {
    events: mpsc::Sender<ProviderEvent>,
    entered: mpsc::Sender<()>,
    gate: Arc<Gate>,
}

impl EventObserver for BlockingCollector {
    fn provider_event(&self, event: ProviderEvent) {
        self.events.send(event).unwrap();
        self.entered.send(()).unwrap();
        self.gate.wait();
    }
}

struct SubscriptionFixture;

impl SubscriptionFixture {
    fn client(subscriptions: usize, events: usize) -> (Arc<Provider<Endpoint>>, Endpoint) {
        let (client, server) = Endpoint::pair(1);
        let limits = ClientLimits::with_subscriptions(64, 4, subscriptions, events).unwrap();
        (Arc::new(Provider::new(client, limits).unwrap()), server)
    }

    fn identity(generation: u32) -> SubscriptionIdentity {
        SubscriptionIdentity::new(77, generation).unwrap()
    }
}

#[test]
fn event_and_out() {
    let (provider, server) = SubscriptionFixture::client(2, 4);
    let (event_tx, event_rx) = mpsc::channel();
    let subscription = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"poll",
            Arc::new(Collector { events: event_tx }),
        )
        .unwrap();
    let subscribe = server.receive_frame();
    assert_eq!(subscribe.0, FrameKind::Subscribe);
    assert_eq!(subscribe.2, b"poll");

    let ticket = provider.begin(b"request").unwrap();
    let request = server.receive_frame();
    server.send_frame(FrameKind::Event, subscribe.1, b"ready");
    server.send_frame(FrameKind::Reply, request.1, b"reply");
    assert_eq!(provider.wait(ticket).unwrap().payload, b"reply");
    assert_eq!(event_rx.recv().unwrap().payload, b"ready");

    subscription.unsubscribe().unwrap();
    assert_eq!(server.receive_frame().0, FrameKind::Unsubscribe);
    provider.close();
}

#[test]
fn bounded_queue_coalesces() {
    let (provider, server) = SubscriptionFixture::client(1, 2);
    let (event_tx, event_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let gate = Arc::new(Gate::new());
    let subscription = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"poll",
            Arc::new(BlockingCollector {
                events: event_tx,
                entered: entered_tx,
                gate: Arc::clone(&gate),
            }),
        )
        .unwrap();
    let id = server.receive_frame().1;
    server.send_frame(FrameKind::Event, id, b"e1");
    entered_rx.recv().unwrap();
    for payload in [b"e2".as_slice(), b"e3", b"e4"] {
        server.send_frame(FrameKind::Event, id, payload);
    }
    let barrier = provider.begin(b"barrier").unwrap();
    let request = server.receive_frame();
    server.send_frame(FrameKind::Reply, request.1, b"done");
    provider.wait(barrier).unwrap();
    gate.release();

    let first = event_rx.recv().unwrap();
    let second = event_rx.recv().unwrap();
    let third = event_rx.recv().unwrap();
    assert_eq!(first.payload, b"e1");
    assert_eq!(second.payload, b"e2");
    assert_eq!(third.payload, b"e4");
    assert_eq!(first.lost + second.lost + third.lost, 1);

    subscription.unsubscribe().unwrap();
    server.receive_frame();
    provider.close();
}

#[test]
fn unsubscribe_waits_for() {
    let (provider, server) = SubscriptionFixture::client(1, 2);
    let (event_tx, _event_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let gate = Arc::new(Gate::new());
    let subscription = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"poll",
            Arc::new(BlockingCollector {
                events: event_tx,
                entered: entered_tx,
                gate: Arc::clone(&gate),
            }),
        )
        .unwrap();
    let id = server.receive_frame().1;
    server.send_frame(FrameKind::Event, id, b"busy");
    entered_rx.recv().unwrap();
    let unsubscribe = thread::spawn(move || subscription.unsubscribe());
    assert_eq!(server.receive_frame().0, FrameKind::Unsubscribe);
    server.send_frame(FrameKind::Event, id, b"late");
    gate.release();
    assert_eq!(unsubscribe.join().unwrap(), Ok(()));

    let ticket = provider.begin(b"barrier").unwrap();
    let request = server.receive_frame();
    server.send_frame(FrameKind::Reply, request.1, b"done");
    provider.wait(ticket).unwrap();
    assert_eq!(provider.stale_events(), 1);
    provider.close();
}

#[test]
fn checkpoint_rejects_active() {
    let (provider, server) = SubscriptionFixture::client(1, 2);
    let (event_tx, _event_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let gate = Arc::new(Gate::new());
    let subscription = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"poll",
            Arc::new(BlockingCollector {
                events: event_tx,
                entered: entered_tx,
                gate: Arc::clone(&gate),
            }),
        )
        .unwrap();
    let id = server.receive_frame().1;
    server.send_frame(FrameKind::Event, id, b"busy");
    entered_rx.recv().unwrap();
    let checkpoint_provider = provider.clone();
    let freeze = thread::spawn(move || checkpoint_provider.freeze_checkpoint());
    gate.release();
    freeze.join().unwrap().unwrap();
    provider.checkpoint_client().unwrap();
    provider.thaw_checkpoint();
    let unsubscribe = thread::spawn(move || subscription.unsubscribe());
    assert_eq!(server.receive_frame().0, FrameKind::Unsubscribe);
    unsubscribe.join().unwrap().unwrap();
    provider.close();
}

#[test]
fn identity_generation_reuse() {
    let (provider, server) = SubscriptionFixture::client(1, 2);
    let (first_tx, _first_rx) = mpsc::channel();
    let first = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"first",
            Arc::new(Collector { events: first_tx }),
        )
        .unwrap();
    let old_id = server.receive_frame().1;
    first.unsubscribe().unwrap();
    server.receive_frame();

    let (second_tx, second_rx) = mpsc::channel();
    let second = provider
        .subscribe(
            SubscriptionFixture::identity(2),
            b"second",
            Arc::new(Collector { events: second_tx }),
        )
        .unwrap();
    let new_id = server.receive_frame().1;
    assert_ne!(old_id, new_id);
    server.send_frame(FrameKind::Event, old_id, b"stale");
    server.send_frame(FrameKind::Event, new_id, b"current");
    assert_eq!(second_rx.recv().unwrap().payload, b"current");
    assert_eq!(provider.stale_events(), 1);
    second.unsubscribe().unwrap();
    server.receive_frame();
    provider.close();
}

#[test]
fn subscription_capacity_duplicate() {
    let (provider, server) = SubscriptionFixture::client(1, 1);
    let (events, _receiver) = mpsc::channel();
    let first = provider
        .subscribe(
            SubscriptionFixture::identity(1),
            b"one",
            Arc::new(Collector { events: events.clone() }),
        )
        .unwrap();
    server.receive_frame();
    assert!(matches!(
        provider.subscribe(
            SubscriptionFixture::identity(1),
            b"duplicate",
            Arc::new(Collector { events: events.clone() })
        ),
        Err(ProviderError::DuplicateSubscription)
    ));
    assert!(matches!(
        provider.subscribe(
            SubscriptionFixture::identity(2),
            b"full",
            Arc::new(Collector { events })
        ),
        Err(ProviderError::Capacity)
    ));
    let snapshots = provider.subscription_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].identity, SubscriptionFixture::identity(1));
    first.unsubscribe().unwrap();
    server.receive_frame();
    provider.close();
}
