use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hl_descriptor::{
    DescriptorCheckpointError, DescriptorFlags, DescriptorObjectCheckpoint, DescriptorTable, ObjectKind,
    OpenDescriptionImage, OpenFileDescription,
};

use crate::{
    ControlCodec, ControlError, ControlMessage, ControlWord, QueueSnapshot, SenderCredentials, UnixMessageQueue,
};

#[derive(Debug, Default)]
struct Lifecycle {
    closed: AtomicUsize,
}

impl OpenFileDescription for Lifecycle {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn close(&self) {
        self.closed.fetch_add(1, Ordering::AcqRel);
    }
}

struct LifecycleCodec;

impl DescriptorObjectCheckpoint for LifecycleCodec {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(1)
    }

    fn snapshot_into(
        &self,
        _: u64,
        _: &dyn OpenFileDescription,
        output: &mut [u8],
    ) -> Result<(), DescriptorCheckpointError> {
        if output.len() != 1 {
            return Err(DescriptorCheckpointError::Object);
        }
        output[0] = 1;
        Ok(())
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::File || description.object != [1] {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(Arc::new(Lifecycle::default()))
    }
}

#[test]
fn queued_right_outlives() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(8).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"message".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor, descriptor])],
        )
        .unwrap();
    sender.close(descriptor).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 0);

    let receiver = DescriptorTable::new(8).unwrap();
    let (_, control) = queue.receive(&receiver, 2, true).unwrap().unwrap();
    assert_eq!(control.descriptors.len(), 2);
    receiver.close(control.descriptors[0]).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 0);
    receiver.close(control.descriptors[1]).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn closed_sender_checkpoint() {
    let sender = DescriptorTable::new(8).unwrap();
    let descriptor = sender
        .install(0, Arc::new(Lifecycle::default()), DescriptorFlags::default())
        .unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"queued".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor])],
        )
        .unwrap();
    let queue_image = queue.snapshot();
    sender.close(descriptor).unwrap();
    sender.freeze_checkpoint();
    let descriptor_image = sender.checkpoint_image(&LifecycleCodec).unwrap();
    sender.thaw_checkpoint();
    assert!(descriptor_image.entries.is_empty());
    assert_eq!(descriptor_image.descriptions.len(), 1);

    let restored = DescriptorTable::restore_checkpoint(&descriptor_image, &LifecycleCodec).unwrap();
    restored.freeze_checkpoint();
    let restored_queue = UnixMessageQueue::restore(&queue_image, |identity| {
        restored.export_checkpoint_identity(identity).ok()
    })
    .unwrap();
    restored.release_checkpoint_roots();
    restored.thaw_checkpoint();
    let receiver = DescriptorTable::new(8).unwrap();
    let (_, control) = restored_queue.receive(&receiver, 1, false).unwrap().unwrap();
    assert_eq!(control.descriptors.len(), 1);
}

#[test]
fn checkpoint_restores_groups() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let retained = sender.export_description(descriptor).unwrap();
    let identity = retained.identity();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"restored".to_vec(),
            vec![
                ControlMessage::Rights(vec![descriptor, descriptor]),
                ControlMessage::Unknown {
                    level: 7,
                    kind: 8,
                    data: vec![9],
                },
                ControlMessage::Rights(vec![descriptor]),
            ],
        )
        .unwrap();
    let snapshot = queue.snapshot();
    let restored =
        UnixMessageQueue::restore(&snapshot, |requested| (requested == identity).then(|| retained.clone())).unwrap();
    drop(queue);
    sender.close(descriptor).unwrap();
    drop(retained);

    let receiver = DescriptorTable::new(4).unwrap();
    let received = restored
        .receive_transactional_capacity(&receiver, 128, true, |payload, control| {
            assert_eq!(payload, b"restored");
            assert_eq!(control.descriptors, [0, 1, 2]);
            assert_eq!(
                control.controls,
                [
                    ControlMessage::Rights(vec![0, 1]),
                    ControlMessage::Unknown {
                        level: 7,
                        kind: 8,
                        data: vec![9],
                    },
                    ControlMessage::Rights(vec![2]),
                ]
            );
            Ok(())
        })
        .unwrap()
        .unwrap();
    for descriptor in received.descriptors {
        assert_eq!(
            receiver.flags(descriptor).unwrap().bits() & DescriptorFlags::CLOSE_ON_EXEC,
            DescriptorFlags::CLOSE_ON_EXEC
        );
        receiver.close(descriptor).unwrap();
    }
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn invalid_later_source() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();

    assert_eq!(
        queue.send(
            &sender,
            b"rejected".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor, 99])],
        ),
        Err(ControlError::BadDescriptor),
    );
    assert!(queue.snapshot().messages.is_empty());
    sender.close(descriptor).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn aggregate_rights_limit() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    let controls = vec![
        ControlMessage::Rights(vec![descriptor; 126]),
        ControlMessage::Rights(vec![descriptor; 128]),
    ];

    assert_eq!(
        queue.send(&sender, b"rejected".to_vec(), controls),
        Err(ControlError::TooBig),
    );
    assert!(queue.snapshot().messages.is_empty());
    sender.close(descriptor).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn destination_batch_capacity() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"consumed".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor, descriptor])],
        )
        .unwrap();
    let receiver = DescriptorTable::new(1).unwrap();

    assert_eq!(
        queue.receive_transactional(&receiver, true, |_, _| panic!(
            "copyout must not run without the complete fd batch"
        ),),
        Err(ControlError::TooManyOpenFiles),
    );
    assert!(receiver.is_empty());
    assert!(queue.snapshot().messages.is_empty());
    sender.close(descriptor).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn peek_capacity_preserves() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"peekable".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor, descriptor])],
        )
        .unwrap();
    sender.close(descriptor).unwrap();
    let too_small = DescriptorTable::new(1).unwrap();

    assert_eq!(
        queue.peek_transactional_capacity(&too_small, 64, false, |_, _| panic!(
            "copyout must not run without the complete fd batch"
        ),),
        Err(ControlError::TooManyOpenFiles),
    );
    assert!(too_small.is_empty());
    assert_eq!(queue.snapshot().messages.len(), 1);
    assert_eq!(object.closed.load(Ordering::Acquire), 0);

    let receiver = DescriptorTable::new(2).unwrap();
    let control = queue
        .peek_transactional_capacity(&receiver, 64, false, |payload, staged| {
            assert_eq!(payload, b"peekable");
            assert_eq!(staged.descriptors, [0, 1]);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(control.descriptors, [0, 1]);
    assert_eq!(queue.snapshot().messages.len(), 1);
    receiver.close(0).unwrap();
    receiver.close(1).unwrap();
    drop(queue);
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn truncation_delivers_the() {
    let sender = DescriptorTable::new(4).unwrap();
    let descriptor = sender
        .install(0, Arc::new(Lifecycle::default()), DescriptorFlags::default())
        .unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            Vec::new(),
            vec![ControlMessage::Rights(vec![descriptor, descriptor])],
        )
        .unwrap();
    let receiver = DescriptorTable::new(4).unwrap();
    let (_, control) = queue.receive(&receiver, 1, false).unwrap().unwrap();
    assert!(control.truncated);
    assert_eq!(control.descriptors.len(), 1);
    assert!(receiver.pin(control.descriptors[0]).is_ok());
    receiver.close(control.descriptors[0]).unwrap();
    sender.close(descriptor).unwrap();
}

#[test]
fn codec_rejects_truncated() {
    assert_eq!(
        ControlCodec::decode(&[0; 11], ControlWord::Four),
        Err(ControlError::Invalid)
    );
    let mut malformed = vec![0; 15];
    malformed[..4].copy_from_slice(&15_u32.to_le_bytes());
    malformed[4..8].copy_from_slice(&1_i32.to_le_bytes());
    malformed[8..12].copy_from_slice(&1_i32.to_le_bytes());
    assert_eq!(
        ControlCodec::decode(&malformed, ControlWord::Four),
        Err(ControlError::Invalid)
    );
}

#[test]
fn codec_roundtrips_mixed() {
    let controls = vec![
        ControlMessage::Rights(vec![3, 9]),
        ControlMessage::Credentials {
            process: 42,
            user: 7,
            group: 8,
        },
        ControlMessage::Unknown {
            level: 313,
            kind: 99,
            data: vec![1, 2, 3, 4, 5],
        },
    ];
    for word in [ControlWord::Four, ControlWord::Eight] {
        let encoded = ControlCodec::encode(&controls, word, 256).unwrap();
        assert!(!encoded.truncated);
        assert_eq!(ControlCodec::decode(&encoded.bytes, word).unwrap(), controls);
        assert_eq!(
            ControlCodec::encode(&ControlCodec::decode(&encoded.bytes, word).unwrap(), word, 256,)
                .unwrap()
                .bytes,
            encoded.bytes,
        );
    }
}

#[test]
fn encoder_delivers_only() {
    let controls = [ControlMessage::Rights(vec![1, 2, 3])];
    let encoded = ControlCodec::encode(&controls, ControlWord::Eight, 24).unwrap();
    assert!(encoded.truncated);
    assert_eq!(
        ControlCodec::decode(&encoded.bytes, ControlWord::Eight).unwrap(),
        vec![ControlMessage::Rights(vec![1, 2])],
    );
    assert_eq!(encoded.bytes.len(), 24);
}

#[test]
fn control_unpadded_tail() {
    let controls = [ControlMessage::Rights(vec![7])];
    for capacity in 20..24 {
        let encoded = ControlCodec::encode(&controls, ControlWord::Eight, capacity).unwrap();
        assert_eq!(encoded.bytes.len(), 20);
        assert!(!encoded.truncated);
        assert_eq!(
            ControlCodec::decode(&encoded.bytes, ControlWord::Eight).unwrap(),
            controls
        );
    }
}

#[test]
fn unpadded_prefix_reports() {
    let controls = [
        ControlMessage::Rights(vec![7]),
        ControlMessage::Credentials {
            process: 1,
            user: 2,
            group: 3,
        },
    ];
    let encoded = ControlCodec::encode(&controls, ControlWord::Eight, 20).unwrap();
    assert_eq!(encoded.bytes.len(), 20);
    assert!(encoded.truncated);
    assert_eq!(
        ControlCodec::decode(&encoded.bytes, ControlWord::Eight).unwrap(),
        vec![ControlMessage::Rights(vec![7])],
    );
}

#[test]
fn credential_spoofing_is() {
    let sender = DescriptorTable::new(1).unwrap();
    let queue = UnixMessageQueue::new();
    let claimed = ControlMessage::Credentials {
        process: 11,
        user: 22,
        group: 33,
    };
    assert_eq!(
        queue.send_authenticated(
            &sender,
            Vec::new(),
            vec![claimed.clone()],
            Some(SenderCredentials {
                process: 11,
                user: 99,
                group: 33
            }),
        ),
        Err(ControlError::PermissionDenied),
    );
    queue
        .send_authenticated(
            &sender,
            Vec::new(),
            vec![claimed.clone()],
            Some(SenderCredentials {
                process: 11,
                user: 22,
                group: 33,
            }),
        )
        .unwrap();
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.messages.len(), 1);
    let mut tampered = snapshot.clone();
    tampered.messages[0].credentials = Some(SenderCredentials {
        process: 11,
        user: 23,
        group: 33,
    });
    assert_eq!(
        UnixMessageQueue::restore(&tampered, |_| None).err(),
        Some(ControlError::PermissionDenied),
    );
    let receiver = DescriptorTable::new(1).unwrap();
    let (_, projected) = queue.receive(&receiver, 0, false).unwrap().unwrap();
    assert_eq!(projected.controls, vec![claimed]);
}

#[test]
fn queue_snapshot_uses() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let retained = sender.export_description(descriptor).unwrap();
    let identity = retained.identity();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"checkpoint".to_vec(),
            vec![
                ControlMessage::Unknown {
                    level: 7,
                    kind: 8,
                    data: vec![9],
                },
                ControlMessage::Rights(vec![descriptor]),
            ],
        )
        .unwrap();
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.messages[0].rights[0].identities, vec![identity]);
    assert_eq!(snapshot.messages[0].controls[1], ControlMessage::Rights(Vec::new()),);
    assert_eq!(
        UnixMessageQueue::restore(&snapshot, |_| None).err(),
        Some(ControlError::MissingDescription),
    );

    let restored =
        UnixMessageQueue::restore(&snapshot, |requested| (requested == identity).then(|| retained.clone())).unwrap();
    drop(queue);
    sender.close(descriptor).unwrap();
    drop(retained);
    assert_eq!(object.closed.load(Ordering::Acquire), 0);
    let receiver = DescriptorTable::new(2).unwrap();
    let (_, control) = restored.receive(&receiver, 1, false).unwrap().unwrap();
    receiver.close(control.descriptors[0]).unwrap();
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn restore_rejects_unbounded() {
    let mut snapshot = QueueSnapshot { messages: Vec::new() };
    snapshot.messages.resize_with(1025, || crate::QueueMessageSnapshot {
        payload: Vec::new(),
        controls: Vec::new(),
        rights: Vec::new(),
        credentials: None,
        automatic: false,
    });
    let mut called = false;
    assert_eq!(
        UnixMessageQueue::restore(&snapshot, |_| {
            called = true;
            None
        })
        .err(),
        Some(ControlError::TooBig),
    );
    assert!(!called);

    let snapshot = QueueSnapshot {
        messages: vec![crate::QueueMessageSnapshot {
            payload: Vec::new(),
            controls: vec![ControlMessage::Rights(Vec::new()), ControlMessage::Rights(Vec::new())],
            rights: vec![
                crate::QueueRightsSnapshot {
                    identities: vec![1; 126],
                },
                crate::QueueRightsSnapshot {
                    identities: vec![1; 128],
                },
            ],
            credentials: None,
            automatic: false,
        }],
    };
    assert_eq!(
        UnixMessageQueue::restore(&snapshot, |_| panic!("must validate first")).err(),
        Some(ControlError::TooBig),
    );
}

#[test]
fn restore_rejects_shape() {
    let snapshot = QueueSnapshot {
        messages: vec![crate::QueueMessageSnapshot {
            payload: Vec::new(),
            controls: vec![ControlMessage::Rights(Vec::new())],
            rights: Vec::new(),
            credentials: None,
            automatic: false,
        }],
    };
    assert_eq!(
        UnixMessageQueue::restore(&snapshot, |_| panic!("must validate first")).err(),
        Some(ControlError::Invalid),
    );
}

#[test]
fn copyout_fault_rolls() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(4).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(
            &sender,
            b"consumed".to_vec(),
            vec![ControlMessage::Rights(vec![descriptor])],
        )
        .unwrap();
    sender.close(descriptor).unwrap();
    let receiver = DescriptorTable::new(4).unwrap();
    assert_eq!(
        queue.receive_transactional(&receiver, false, |payload, numbers| {
            assert_eq!(payload, b"consumed");
            assert_eq!(numbers.len(), 1);
            Err(ControlError::Fault)
        }),
        Err(ControlError::Fault)
    );
    assert!(receiver.pin(0).is_err());
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
    assert!(
        queue
            .receive_transactional(&receiver, false, |_, _| Ok(()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn copyout_cancel_rolls() {
    let object = Arc::new(Lifecycle::default());
    let sender = DescriptorTable::new(2).unwrap();
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = UnixMessageQueue::new();
    queue
        .send(&sender, Vec::new(), vec![ControlMessage::Rights(vec![descriptor])])
        .unwrap();
    sender.close(descriptor).unwrap();
    let receiver = DescriptorTable::new(1).unwrap();

    assert_eq!(
        queue.receive_transactional(&receiver, false, |_, _| Err(ControlError::Canceled),),
        Err(ControlError::Canceled)
    );
    assert!(receiver.is_empty());
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn concurrent_sender_close() {
    let object = Arc::new(Lifecycle::default());
    let sender = Arc::new(DescriptorTable::new(2).unwrap());
    let descriptor = sender.install(0, object.clone(), DescriptorFlags::default()).unwrap();
    let queue = Arc::new(UnixMessageQueue::new());
    queue
        .send(&sender, Vec::new(), vec![ControlMessage::Rights(vec![descriptor])])
        .unwrap();
    let receiver = Arc::new(DescriptorTable::new(2).unwrap());

    let sender_thread = std::thread::spawn({
        let sender = sender.clone();
        move || sender.close(descriptor).unwrap()
    });
    let receiver_thread = std::thread::spawn({
        let queue = queue.clone();
        let receiver = receiver.clone();
        move || {
            let control = queue
                .receive_transactional(&receiver, false, |_, numbers| {
                    assert_eq!(numbers.len(), 1);
                    Ok(())
                })
                .unwrap()
                .unwrap();
            receiver.close(control.descriptors[0]).unwrap();
        }
    });
    sender_thread.join().unwrap();
    receiver_thread.join().unwrap();

    assert!(receiver.is_empty());
    assert_eq!(object.closed.load(Ordering::Acquire), 1);
}

#[test]
fn receiver_close_racing() {
    for _ in 0..32 {
        let blocker = Arc::new(Lifecycle::default());
        let transferred = Arc::new(Lifecycle::default());
        let receiver = Arc::new(DescriptorTable::new(1).unwrap());
        let occupied = receiver
            .install(0, blocker.clone(), DescriptorFlags::default())
            .unwrap();
        let sender = DescriptorTable::new(1).unwrap();
        let source = sender
            .install(0, transferred.clone(), DescriptorFlags::default())
            .unwrap();
        let queue = Arc::new(UnixMessageQueue::new());
        queue
            .send(&sender, Vec::new(), vec![ControlMessage::Rights(vec![source])])
            .unwrap();
        sender.close(source).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let close_thread = std::thread::spawn({
            let barrier = barrier.clone();
            let receiver = receiver.clone();
            move || {
                barrier.wait();
                receiver.close(occupied).unwrap();
            }
        });
        barrier.wait();
        let result = queue.receive_transactional(&receiver, false, |_, _| Ok(()));
        close_thread.join().unwrap();

        match result {
            Ok(Some(control)) => receiver.close(control.descriptors[0]).unwrap(),
            Err(ControlError::TooManyOpenFiles) => assert!(receiver.is_empty()),
            outcome => panic!("unexpected receive outcome: {outcome:?}"),
        }
        assert_eq!(blocker.closed.load(Ordering::Acquire), 1);
        assert_eq!(transferred.closed.load(Ordering::Acquire), 1);
        assert!(receiver.is_empty());
    }
}

#[test]
fn transactional_capacity_preserves() {
    let sender = DescriptorTable::new(4).unwrap();
    let object = Arc::new(Lifecycle::default());
    let rights = (0..3)
        .map(|_| sender.install(0, object.clone(), DescriptorFlags::default()).unwrap())
        .collect::<Vec<_>>();
    let credentials = SenderCredentials {
        process: 7,
        user: 8,
        group: 9,
    };
    let controls = vec![
        ControlMessage::Credentials {
            process: 7,
            user: 8,
            group: 9,
        },
        ControlMessage::Rights(rights),
        ControlMessage::Unknown {
            level: 9,
            kind: 10,
            data: vec![1, 2, 3, 4],
        },
    ];
    let queue = UnixMessageQueue::new();
    queue
        .send_authenticated(&sender, b"ordered".to_vec(), controls, Some(credentials))
        .unwrap();
    let receiver = DescriptorTable::new(4).unwrap();
    let result = queue
        .receive_transactional_capacity(&receiver, 56, false, |payload, staged| {
            assert_eq!(payload, b"ordered");
            assert_eq!(staged.descriptors, [0, 1]);
            assert_eq!(
                staged.controls,
                [
                    ControlMessage::Credentials {
                        process: 7,
                        user: 8,
                        group: 9,
                    },
                    ControlMessage::Rights(vec![0, 1]),
                ],
            );
            assert!(staged.truncated);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(result.descriptors, [0, 1]);
    assert!(receiver.pin(0).is_ok());
    assert!(receiver.pin(1).is_ok());
    assert!(receiver.pin(2).is_err());
}
