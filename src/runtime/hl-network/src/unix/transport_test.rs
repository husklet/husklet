use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError,
    OpenFileDescription, OperationCancellation, Readiness, ReadinessObserver, StatusFlags,
};

use crate::{ControlMessage, SenderCredentials, SocketHostError, SocketType, UnixSocketPair, UnixTransportError};

#[derive(Debug, Default)]
struct Lifetime {
    closes: AtomicUsize,
}

impl OpenFileDescription for Lifetime {
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct Cancellation {
    interrupted: AtomicBool,
    observer: Mutex<Option<Arc<dyn CancellationNotification>>>,
}

struct Subscription;
impl CancellationSubscription for Subscription {}

impl Cancellation {
    fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        if let Some(observer) = self.observer.lock().unwrap().clone() {
            observer.notify();
        }
    }
}

impl OperationCancellation for Cancellation {
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }
    fn subscribe(&self, observer: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        *self.observer.lock().unwrap() = Some(observer);
        Box::new(Subscription)
    }
}

#[derive(Default)]
struct Observer(AtomicUsize);
impl ReadinessObserver for Observer {
    fn readiness_changed(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn stream_socketpair_preserves() {
    let pair = UnixSocketPair::with_capacity(SocketType::Stream, StatusFlags::default(), 4).unwrap();
    assert_eq!(pair.endpoints[0].description.write(b"abcdef").unwrap(), 4);
    let mut first = [0; 2];
    assert_eq!(pair.endpoints[1].description.read(&mut first).unwrap(), 2);
    assert_eq!(&first, b"ab");
    assert_eq!(pair.endpoints[0].description.write(b"ef").unwrap(), 2);
    pair.endpoints[0].shutdown(false, true);
    let mut remaining = [0; 4];
    assert_eq!(pair.endpoints[1].description.read(&mut remaining).unwrap(), 4);
    assert_eq!(&remaining, b"cdef");
    assert_eq!(pair.endpoints[1].description.read(&mut remaining).unwrap(), 0);
    assert_eq!(pair.endpoints[0].description.write(b"x"), Err(ObjectError::BrokenPipe),);
}

#[test]
fn datagram_peer_shutdown_or_close_does_not_synthesize_eof() {
    let pair = UnixSocketPair::with_capacity(
        SocketType::Datagram,
        StatusFlags::from_bits(StatusFlags::NONBLOCKING),
        16,
    )
    .unwrap();
    pair.endpoints[0].description.write(b"queued").unwrap();
    pair.endpoints[0].shutdown(false, true);

    let mut payload = [0; 8];
    assert_eq!(pair.endpoints[1].description.read(&mut payload), Ok(6));
    assert_eq!(&payload[..6], b"queued");
    assert_eq!(
        pair.endpoints[1].description.read(&mut payload),
        Err(ObjectError::WouldBlock),
    );
    let peer_readiness = pair.endpoints[1]
        .description
        .readiness(Readiness::from_bits(Readiness::READ | Readiness::WRITE));
    assert!(!peer_readiness.contains(Readiness::READ));
    assert!(peer_readiness.contains(Readiness::WRITE));
    assert_eq!(pair.endpoints[0].description.write(b"x"), Err(ObjectError::BrokenPipe));

    pair.endpoints[0].description.close();
    assert_eq!(
        pair.endpoints[1].description.read(&mut payload),
        Err(ObjectError::WouldBlock),
    );
}

#[test]
fn record_socketpair_is() {
    let pair = UnixSocketPair::with_capacity(
        SocketType::SequencePacket,
        StatusFlags::from_bits(StatusFlags::NONBLOCKING),
        5,
    )
    .unwrap();
    assert_eq!(pair.endpoints[0].description.write(b"hello").unwrap(), 5);
    assert_eq!(pair.endpoints[0].description.write(b"x"), Err(ObjectError::WouldBlock),);
    let mut short = [0; 3];
    assert_eq!(pair.endpoints[1].description.read(&mut short).unwrap(), 3);
    assert_eq!(&short, b"hel");
    assert_eq!(pair.endpoints[0].description.write(b"world").unwrap(), 5);
}

#[test]
fn mixed_data_and() {
    let object = Arc::new(Lifetime::default());
    let sender = DescriptorTable::new(2).unwrap();
    let source = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let pair = UnixSocketPair::with_capacity(SocketType::Datagram, StatusFlags::default(), 4).unwrap();
    pair.endpoints[0]
        .send_message(
            &sender,
            b"data".to_vec(),
            vec![ControlMessage::Rights(vec![source])],
            None,
            true,
        )
        .unwrap();
    assert_eq!(
        pair.endpoints[0].send_message(&sender, vec![1], Vec::new(), None, true,),
        Err(UnixTransportError::WouldBlock),
    );
    sender.close(source).unwrap();
    assert_eq!(object.closes.load(Ordering::Acquire), 0);
    let receiver = DescriptorTable::new(2).unwrap();
    let (payload, control) = pair.endpoints[1].receive_message(&receiver, 1, false).unwrap().unwrap();
    assert_eq!(payload, b"data");
    assert_eq!(
        control.controls,
        vec![ControlMessage::Rights(control.descriptors.clone())],
    );
    receiver.close(control.descriptors[0]).unwrap();
    assert_eq!(object.closes.load(Ordering::Acquire), 1);
}

#[test]
fn ancillary_send_publishes_readiness() {
    let sender = DescriptorTable::new(1).unwrap();
    let pair = UnixSocketPair::new(SocketType::SequencePacket, StatusFlags::default()).unwrap();
    let observer = Arc::new(Observer::default());
    let _subscription = pair.endpoints[1]
        .description
        .subscribe_readiness(observer.clone())
        .unwrap();

    pair.endpoints[0]
        .send_message(&sender, b"message".to_vec(), Vec::new(), None, true)
        .unwrap();

    assert!(observer.0.load(Ordering::Acquire) > 0);
    assert!(
        pair.endpoints[1]
            .description
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
}

#[test]
fn readiness_tracks_buffer() {
    let pair = UnixSocketPair::with_capacity(SocketType::Stream, StatusFlags::default(), 1).unwrap();
    assert!(
        pair.endpoints[0]
            .description
            .readiness(Readiness::from_bits(Readiness::WRITE))
            .contains(Readiness::WRITE)
    );
    pair.endpoints[0].description.write(b"x").unwrap();
    assert!(
        !pair.endpoints[0]
            .description
            .readiness(Readiness::from_bits(Readiness::WRITE))
            .contains(Readiness::WRITE)
    );
    pair.endpoints[0].shutdown(false, true);
    assert!(
        pair.endpoints[1]
            .description
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
}

#[test]
fn readiness_transitions() {
    let pair = UnixSocketPair::with_capacity(SocketType::Stream, StatusFlags::default(), 1).unwrap();
    let observer = Arc::new(Observer::default());
    let _subscription = pair.endpoints[1]
        .description
        .subscribe_readiness(observer.clone())
        .unwrap();
    pair.endpoints[0].description.write(b"x").unwrap();
    let mut byte = [0];
    pair.endpoints[1].description.read(&mut byte).unwrap();
    pair.endpoints[0].shutdown(false, true);
    assert!(observer.0.load(Ordering::Acquire) >= 3);
    let before = observer.0.load(Ordering::Acquire);
    pair.endpoints[1].description.close();
    pair.endpoints[0].description.write(b"y").ok();
    assert_eq!(observer.0.load(Ordering::Acquire), before);
}

#[test]
fn cancellation_ordering() {
    let pair = UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap();
    let cancellation = Arc::new(Cancellation::default());
    cancellation.interrupt();
    let mut byte = [0];
    assert_eq!(
        pair.endpoints[1]
            .description
            .read_with_cancellation(&mut byte, cancellation.as_ref()),
        Err(ObjectError::Interrupted)
    );

    let cancellation = Arc::new(Cancellation::default());
    pair.endpoints[0].description.write(b"d").unwrap();
    cancellation.interrupt();
    assert_eq!(
        pair.endpoints[1]
            .description
            .read_with_cancellation(&mut byte, cancellation.as_ref()),
        Ok(1)
    );
    assert_eq!(byte, *b"d");
}

#[test]
fn stream_peek_preserves() {
    let pair = UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap();
    pair.endpoints[0].description.write(b"peek-data").unwrap();
    let mut peeked = [0_u8; 9];
    assert_eq!(pair.endpoints[1].peek(&mut peeked, true), Ok(9));
    assert_eq!(&peeked, b"peek-data");
    let mut repeated = [0_u8; 9];
    assert_eq!(pair.endpoints[1].peek(&mut repeated, true), Ok(9));
    assert_eq!(&repeated, b"peek-data");
    let mut consumed = [0_u8; 9];
    assert_eq!(pair.endpoints[1].description.read(&mut consumed), Ok(9));
    assert_eq!(&consumed, b"peek-data");
    assert_eq!(
        pair.endpoints[1].peek(&mut consumed, true),
        Err(SocketHostError::WouldBlock),
    );
}

#[test]
fn cancellation_wakes() {
    let pair = UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap();
    let description = pair.endpoints[1].description.clone();
    let cancellation = Arc::new(Cancellation::default());
    let thread_cancellation = cancellation.clone();
    let blocked =
        std::thread::spawn(move || description.read_with_cancellation(&mut [0], thread_cancellation.as_ref()));
    while cancellation.observer.lock().unwrap().is_none() {
        std::thread::yield_now();
    }
    cancellation.interrupt();
    assert_eq!(blocked.join().unwrap(), Err(ObjectError::Interrupted));
}

#[test]
fn checkpoint_restore_rebinds() {
    let object = Arc::new(Lifetime::default());
    let sender = DescriptorTable::new(2).unwrap();
    let source = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let retained = sender.export_description(source).unwrap();
    let identity = retained.identity();
    let pair = UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap();
    let peer_credentials = SenderCredentials {
        process: 31,
        user: 37,
        group: 41,
    };
    pair.set_peer_credentials(peer_credentials);
    pair.endpoints[1].set_passcred(true);
    pair.endpoints[0].description.write(b"bytes").unwrap();
    pair.endpoints[0]
        .send_message(
            &sender,
            b"control".to_vec(),
            vec![ControlMessage::Rights(vec![source, source])],
            Some(SenderCredentials {
                process: 17,
                user: 23,
                group: 29,
            }),
            false,
        )
        .unwrap();
    let snapshot = pair.snapshot();
    assert!(snapshot.endpoints[1].passcred);
    assert_eq!(snapshot.endpoints[1].peer_credentials, Some(peer_credentials));
    assert!(snapshot.endpoints[1].ancillary.messages[0].automatic);
    drop(pair);
    sender.close(source).unwrap();
    let restored = UnixSocketPair::restore(&snapshot, StatusFlags::default(), |requested| {
        (requested == identity).then(|| retained.clone())
    })
    .unwrap();
    drop(retained);
    let mut bytes = [0; 5];
    restored.endpoints[1].description.read(&mut bytes).unwrap();
    assert_eq!(&bytes, b"bytes");
    let receiver = DescriptorTable::new(2).unwrap();
    let (_, control) = restored.endpoints[1]
        .receive_message(&receiver, 2, false)
        .unwrap()
        .unwrap();
    assert!(restored.endpoints[1].passcred());
    assert_eq!(restored.peer_credentials(1), Some(peer_credentials));
    assert_eq!(
        control.controls[0],
        ControlMessage::Credentials {
            process: 17,
            user: 23,
            group: 29,
        },
    );
    assert_eq!(control.descriptors.len(), 2);
    receiver.close(control.descriptors[0]).unwrap();
    receiver.close(control.descriptors[1]).unwrap();
    assert_eq!(object.closes.load(Ordering::Acquire), 1);
}

#[test]
fn passcred_enqueue_peek() {
    verify_passcred_queue(SocketType::Stream);
    verify_passcred_queue(SocketType::Datagram);
    verify_passcred_queue(SocketType::SequencePacket);
}

fn verify_passcred_queue(socket_type: SocketType) {
    let pair = UnixSocketPair::new(socket_type, StatusFlags::default()).unwrap();
    let table = DescriptorTable::new(1).unwrap();
    let credentials = SenderCredentials {
        process: 31,
        user: 37,
        group: 41,
    };
    pair.endpoints[0]
        .send_message(&table, b"off".to_vec(), Vec::new(), Some(credentials), false)
        .unwrap();
    pair.endpoints[1].set_passcred(true);
    let (payload, control) = pair.endpoints[1].receive_message(&table, 0, false).unwrap().unwrap();
    assert_eq!(payload, b"off");
    assert!(control.controls.is_empty());

    pair.endpoints[0]
        .send_message(&table, b"on".to_vec(), Vec::new(), Some(credentials), false)
        .unwrap();
    pair.endpoints[1].set_passcred(false);
    for peek in [true, true, false] {
        let observed = std::cell::RefCell::new(Vec::new());
        pair.endpoints[1]
            .receive_message_transactional(&table, 32, false, peek, |payload, control| {
                assert_eq!(payload, b"on");
                observed.replace(control.controls.clone());
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            observed.into_inner(),
            [ControlMessage::Credentials {
                process: 31,
                user: 37,
                group: 41,
            }],
        );
    }
}

#[test]
fn passcred_rights_capacity() {
    let object = Arc::new(Lifetime::default());
    let sender = DescriptorTable::new(1).unwrap();
    let source = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let pair = UnixSocketPair::new(SocketType::SequencePacket, StatusFlags::default()).unwrap();
    pair.endpoints[1].set_passcred(true);
    pair.endpoints[0]
        .send_message(
            &sender,
            b"tiny".to_vec(),
            vec![ControlMessage::Rights(vec![source])],
            Some(SenderCredentials {
                process: 43,
                user: 47,
                group: 53,
            }),
            false,
        )
        .unwrap();
    let receiver = DescriptorTable::new(1).unwrap();
    let control = pair.endpoints[1]
        .receive_message_transactional(&receiver, 32, false, false, |payload, control| {
            assert_eq!(payload, b"tiny");
            assert_eq!(
                control.controls,
                [ControlMessage::Credentials {
                    process: 43,
                    user: 47,
                    group: 53,
                }],
            );
            assert!(!control.truncated);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(control.descriptors.is_empty());
    assert!(receiver.is_empty());
    sender.close(source).unwrap();
    assert_eq!(object.closes.load(Ordering::Acquire), 1);
}

#[test]
fn credentials_after_wait() {
    let pair = Arc::new(UnixSocketPair::with_capacity(SocketType::SequencePacket, StatusFlags::default(), 1).unwrap());
    let sender = Arc::new(DescriptorTable::new(1).unwrap());
    pair.endpoints[1].set_passcred(true);
    pair.endpoints[0]
        .send_message(&sender, vec![1], Vec::new(), None, false)
        .unwrap();
    let user = Arc::new(AtomicUsize::new(59));
    let blocked = std::thread::spawn({
        let pair = pair.clone();
        let sender = sender.clone();
        let user = user.clone();
        move || {
            pair.endpoints[0].send_message_with(
                &sender,
                vec![2],
                Vec::new(),
                || {
                    Some(SenderCredentials {
                        process: 61,
                        user: user.load(Ordering::Acquire) as u32,
                        group: 67,
                    })
                },
                false,
            )
        }
    });
    std::thread::yield_now();
    user.store(71, Ordering::Release);
    let receiver = DescriptorTable::new(1).unwrap();
    pair.endpoints[1].receive_message(&receiver, 0, false).unwrap().unwrap();
    blocked.join().unwrap().unwrap();
    let (_, control) = pair.endpoints[1].receive_message(&receiver, 0, false).unwrap().unwrap();
    assert_eq!(
        control.controls,
        [ControlMessage::Credentials {
            process: 61,
            user: 71,
            group: 67,
        }],
    );
}

#[test]
fn blocking_message_sender() {
    let pair = Arc::new(UnixSocketPair::with_capacity(SocketType::SequencePacket, StatusFlags::default(), 1).unwrap());
    let sender = Arc::new(DescriptorTable::new(1).unwrap());
    pair.endpoints[0]
        .send_message(&sender, vec![1], Vec::new(), None, false)
        .unwrap();
    let blocked = std::thread::spawn({
        let pair = pair.clone();
        let sender = sender.clone();
        move || pair.endpoints[0].send_message(&sender, vec![2], Vec::new(), None, false)
    });
    let receiver = DescriptorTable::new(1).unwrap();
    let (first, _) = pair.endpoints[1].receive_message(&receiver, 0, false).unwrap().unwrap();
    assert_eq!(first, vec![1]);
    assert_eq!(blocked.join().unwrap(), Ok(()));
    let (second, _) = pair.endpoints[1].receive_message(&receiver, 0, false).unwrap().unwrap();
    assert_eq!(second, vec![2]);
}
