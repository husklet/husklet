use super::*;
use std::sync::Barrier;

struct TestClock(std::time::Instant);

impl hl_time::MonotonicClock for TestClock {
    fn monotonic_now(&self) -> Result<hl_time::MonotonicInstant, hl_time::ClockError> {
        Ok(hl_time::MonotonicInstant::from_nanoseconds(
            self.0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
        ))
    }
}

struct QueueWait {
    interruption: Arc<hl_sync::Interruption>,
    rendezvous: Option<[Arc<Barrier>; 2]>,
    clock: Arc<TestClock>,
}

impl crate::SocketWait for QueueWait {
    fn interruption(&self) -> Arc<hl_sync::Interruption> {
        self.interruption.clone()
    }

    fn monotonic_now(&self) -> Result<hl_time::MonotonicInstant, hl_time::ClockError> {
        hl_time::MonotonicClock::monotonic_now(self.clock.as_ref())
    }

    fn wait(
        &self,
        queue: &hl_sync::WaitQueue,
        observed: u64,
        deadline: Option<hl_time::Deadline>,
    ) -> Result<hl_sync::WaitOutcome, hl_sync::WaitError> {
        if let Some([entered, sent]) = &self.rendezvous {
            entered.wait();
            sent.wait();
        }
        queue.wait(observed, &self.interruption, deadline, self.clock.as_ref())
    }
}

impl Fixture {
    fn prepare_send(&self, controls: Vec<ControlMessage>, payload: &[u8]) {
        self.memory.put(200, &Self::iovec(250, payload.len() as u64));
        self.memory.put(250, payload);
        let control = ControlCodec::encode(&controls, ControlWord::Eight, 128).unwrap().bytes;
        self.memory.put(128, &Self::message_header(200, 272, control.len()));
        self.memory.put(272, &control);
    }

    fn prepare_receive(&self, control_capacity: usize) {
        self.memory.put(360, &Self::iovec(400, 16));
        self.memory.put(300, &Self::message_header(360, 440, control_capacity));
    }

    fn set_passcred(&self, runtime: &mut RuntimeNetworkSyscalls<Host, Memory>, descriptor: i32, enabled: bool) {
        self.memory.put(480, &i32::from(enabled).to_le_bytes());
        assert_eq!(
            runtime.handle(Self::operation("setsockopt"), [descriptor as u64, 1, 16, 480, 4, 0]),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn recvmsg_consumes_message() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.prepare_send(vec![ControlMessage::Rights(vec![0])], b"fault");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(5),
    );
    fixture.prepare_receive(64);
    fixture.memory.inner.fail_write.store(true, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert!(fixture.descriptors.pin(2).is_err());
    fixture.memory.inner.fail_write.store(false, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x40, 0, 0, 0]),
        LinuxResult::Error(Errno::EAGAIN),
    );
}

#[test]
fn recvmsg_fitting_rights() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.prepare_send(vec![ControlMessage::Rights(vec![0, 0, 0])], b"rights");
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        fixture.prepare_receive(24);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0000, 0, 0, 0],),
            LinuxResult::Value(6),
        );
        assert_ne!(
            fixture.descriptors.flags(2).unwrap().bits() & DescriptorFlags::CLOSE_ON_EXEC,
            0,
        );
        assert_ne!(
            fixture.descriptors.flags(3).unwrap().bits() & DescriptorFlags::CLOSE_ON_EXEC,
            0,
        );
        assert!(fixture.descriptors.pin(4).is_err());
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_ne!(u32::from_le_bytes(bytes[348..352].try_into().unwrap()) & 0x8, 0);
        assert_eq!(
            ControlCodec::decode(&bytes[440..464], ControlWord::Eight).unwrap(),
            [ControlMessage::Rights(vec![2, 3])],
        );
    }
}

#[test]
fn repeated_consuming_receive() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.prepare_send(vec![ControlMessage::Rights(vec![0])], b"peek");
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(4),
        );

        for expected in [2, 3] {
            fixture.prepare_receive(64);
            assert_eq!(
                runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0002, 0, 0, 0],),
                LinuxResult::Value(4),
            );
            assert_ne!(
                fixture.descriptors.flags(expected).unwrap().bits() & DescriptorFlags::CLOSE_ON_EXEC,
                0,
            );
            let bytes = fixture.memory.inner.bytes.lock().unwrap();
            assert_eq!(
                ControlCodec::decode(&bytes[440..464], ControlWord::Eight).unwrap(),
                [ControlMessage::Rights(vec![expected])],
            );
        }

        fixture.prepare_receive(64);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
            LinuxResult::Value(4),
        );
        assert!(fixture.descriptors.pin(4).is_ok());
        fixture.prepare_receive(64);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x40, 0, 0, 0],),
            LinuxResult::Error(Errno::EAGAIN),
        );
    }
}

#[test]
fn failed_message_peekable() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.prepare_send(vec![ControlMessage::Rights(vec![0])], b"retry");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(5),
    );
    fixture.prepare_receive(64);
    fixture.memory.inner.fail_write.store(true, Ordering::Release);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0002, 0, 0, 0],),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert!(fixture.descriptors.pin(2).is_err());

    fixture.memory.inner.fail_write.store(false, Ordering::Release);
    fixture.prepare_receive(64);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0002, 0, 0, 0],),
        LinuxResult::Value(5),
    );
    assert!(fixture.descriptors.pin(2).is_ok());
}

#[test]
fn sendmsg_preserves_record() {
    let fixture = Fixture::new();
    let credentials = hl_network::SenderCredentials {
        process: 41,
        user: 42,
        group: 43,
    };
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64)
        .with_credentials(credentials);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 1, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    let control = ControlMessage::Credentials {
        process: 41,
        user: 42,
        group: 43,
    };
    fixture.prepare_send(vec![control.clone()], b"cred");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(4),
    );
    fixture.prepare_receive(32);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Value(4),
    );
    assert_eq!(
        ControlCodec::decode(
            &fixture.memory.inner.bytes.lock().unwrap()[440..472],
            ControlWord::Eight,
        )
        .unwrap(),
        [control],
    );
}

struct CredentialSource(Mutex<hl_network::SenderCredentials>);

impl SocketCredentials for CredentialSource {
    fn current(&self) -> Option<hl_network::SenderCredentials> {
        Some(*self.0.lock().unwrap())
    }
}

#[test]
fn dynamic_credentials() {
    let fixture = Fixture::new();
    let source = Arc::new(CredentialSource(Mutex::new(hl_network::SenderCredentials {
        process: 71,
        user: 73,
        group: 79,
    })));
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64)
        .with_credential_source(source.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 5, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.set_passcred(&mut runtime, 1, true);
    fixture.prepare_send(Vec::new(), b"first");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(5),
    );
    *source.0.lock().unwrap() = hl_network::SenderCredentials {
        process: 83,
        user: 89,
        group: 97,
    };
    fixture.prepare_send(Vec::new(), b"second");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(6),
    );
    fixture.set_passcred(&mut runtime, 1, false);

    for flags in [0x2, 0x2, 0] {
        fixture.prepare_receive(32);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, flags, 0, 0, 0]),
            LinuxResult::Value(5),
        );
        assert_eq!(
            ControlCodec::decode(
                &fixture.memory.inner.bytes.lock().unwrap()[440..472],
                ControlWord::Eight,
            )
            .unwrap(),
            [ControlMessage::Credentials {
                process: 71,
                user: 73,
                group: 79,
            }],
        );
    }
    fixture.prepare_receive(32);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Value(6),
    );
    assert_eq!(
        ControlCodec::decode(
            &fixture.memory.inner.bytes.lock().unwrap()[440..472],
            ControlWord::Eight,
        )
        .unwrap(),
        [ControlMessage::Credentials {
            process: 83,
            user: 89,
            group: 97,
        }],
    );
}

#[test]
fn forked_recvmsg_blocks() {
    let fixture = Fixture::new();
    let entered = Arc::new(Barrier::new(2));
    let sent = Arc::new(Barrier::new(2));
    let wait = Arc::new(QueueWait {
        interruption: Arc::new(hl_sync::Interruption::new()),
        rendezvous: Some([entered.clone(), sent.clone()]),
        clock: Arc::new(TestClock(std::time::Instant::now())),
    });
    let mut parent = fixture.runtime(GuestArchitecture::Aarch64).with_wait_port(wait);
    assert_eq!(
        parent.handle(Fixture::operation("socketpair"), [1, 5, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.set_passcred(&mut parent, 0, true);
    fixture.prepare_send(Vec::new(), b"forked");
    fixture.prepare_receive(32);

    let child_table = Arc::new(fixture.descriptors.fork());
    assert_eq!(
        fixture.descriptors.snapshot(0).unwrap().description_identity,
        child_table.snapshot(0).unwrap().description_identity,
    );
    fixture.descriptors.close(1).unwrap();
    child_table.close(0).unwrap();
    let mut child = RuntimeNetworkSyscalls::new(
        child_table,
        fixture.catalog.clone(),
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    )
    .with_registry(fixture.sockets.clone())
    .with_credentials(hl_network::SenderCredentials {
        process: 101,
        user: 103,
        group: 107,
    });
    let sender = std::thread::spawn(move || {
        entered.wait();
        let result = child.handle(Fixture::operation("sendmsg"), [1, 128, 0, 0, 0, 0]);
        sent.wait();
        result
    });

    assert_eq!(
        parent.handle(Fixture::operation("recvmsg"), [0, 300, 0, 0, 0, 0]),
        LinuxResult::Value(6),
    );
    assert_eq!(sender.join().unwrap(), LinuxResult::Value(6));
    let bytes = fixture.memory.inner.bytes.lock().unwrap();
    assert_eq!(&bytes[400..406], b"forked");
    assert_eq!(
        ControlCodec::decode(&bytes[440..472], ControlWord::Eight).unwrap(),
        [ControlMessage::Credentials {
            process: 101,
            user: 103,
            group: 107,
        }],
    );
}

#[test]
fn forked_recvmsg_eof() {
    let fixture = Fixture::new();
    let entered = Arc::new(Barrier::new(2));
    let closed = Arc::new(Barrier::new(2));
    let wait = Arc::new(QueueWait {
        interruption: Arc::new(hl_sync::Interruption::new()),
        rendezvous: Some([entered.clone(), closed.clone()]),
        clock: Arc::new(TestClock(std::time::Instant::now())),
    });
    let mut parent = fixture.runtime(GuestArchitecture::Aarch64).with_wait_port(wait);
    assert_eq!(
        parent.handle(Fixture::operation("socketpair"), [1, 5, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.prepare_receive(0);
    let child_table = Arc::new(fixture.descriptors.fork());
    fixture.descriptors.close(1).unwrap();
    child_table.close(0).unwrap();
    let closer = std::thread::spawn(move || {
        entered.wait();
        child_table.close(1).unwrap();
        closed.wait();
    });
    assert_eq!(
        parent.handle(Fixture::operation("recvmsg"), [0, 300, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    closer.join().unwrap();
}

#[test]
fn recvmsg_interrupts() {
    let fixture = Fixture::new();
    let interruption = Arc::new(hl_sync::Interruption::new());
    interruption.interrupt();
    let wait = Arc::new(QueueWait {
        interruption,
        rendezvous: None,
        clock: Arc::new(TestClock(std::time::Instant::now())),
    });
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64).with_wait_port(wait);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 5, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.prepare_receive(0);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EINTR),
    );
}

#[test]
fn recvmsg_timeout() {
    let fixture = Fixture::new();
    let wait = Arc::new(QueueWait {
        interruption: Arc::new(hl_sync::Interruption::new()),
        rendezvous: None,
        clock: Arc::new(TestClock(std::time::Instant::now())),
    });
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64).with_wait_port(wait);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 5, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture
        .memory
        .put(480, &[0_i64.to_le_bytes(), 1_000_i64.to_le_bytes()].concat());
    assert_eq!(
        runtime.handle(Fixture::operation("setsockopt"), [1, 1, 20, 480, 16, 0]),
        LinuxResult::Value(0),
    );
    fixture.prepare_receive(0);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EAGAIN),
    );
}

#[test]
fn host_message_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let LinuxResult::Value(descriptor) = runtime.handle(Fixture::operation("socket"), [2, 2, 17, 0, 0, 0]) else {
            panic!("datagram socket creation failed")
        };
        fixture
            .memory
            .put(64, &[2, 0, 0x1f, 0x90, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        fixture.memory.put(200, &Fixture::iovec(240, 3));
        fixture.memory.put(216, &Fixture::iovec(248, 5));
        fixture.memory.put(240, b"vec");
        fixture.memory.put(248, b"tored");
        let mut send = Fixture::message_header(200, 0, 0);
        send[..8].copy_from_slice(&64_u64.to_le_bytes());
        send[8..12].copy_from_slice(&16_u32.to_le_bytes());
        send[24..32].copy_from_slice(&2_u64.to_le_bytes());
        fixture.memory.put(128, &send);
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [descriptor, 128, 0, 0, 0, 0],),
            LinuxResult::Value(8),
        );
        assert_eq!(fixture.host.state.lock().unwrap().sent_to[0].1, b"vectored");

        fixture.host.state.lock().unwrap().receive_from_data = b"response".to_vec();
        fixture.memory.put(304, &Fixture::iovec(352, 3));
        fixture.memory.put(320, &Fixture::iovec(360, 5));
        let mut receive = Fixture::message_header(304, 0, 0);
        receive[..8].copy_from_slice(&280_u64.to_le_bytes());
        receive[8..12].copy_from_slice(&16_u32.to_le_bytes());
        receive[24..32].copy_from_slice(&2_u64.to_le_bytes());
        fixture.memory.put(400, &receive);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [descriptor, 400, 0, 0, 0, 0],),
            LinuxResult::Value(8),
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(&bytes[352..355], b"res");
        assert_eq!(&bytes[360..365], b"ponse");
        assert_eq!(&bytes[280..282], &[2, 0]);
    }
}

#[test]
fn msg_copied_length() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        for (socket_type, expected) in [(2_u64, 8_u64), (1_u64, 3_u64)] {
            let fixture = Fixture::new();
            let mut runtime = fixture.runtime(architecture);
            assert_eq!(
                runtime.handle(Fixture::operation("socketpair"), [1, socket_type, 0, 32, 0, 0],),
                LinuxResult::Value(0),
            );
            fixture.prepare_send(Vec::new(), b"truncate");
            assert_eq!(
                runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0],),
                LinuxResult::Value(8),
            );
            fixture.memory.put(360, &Fixture::iovec(400, 3));
            fixture.memory.put(300, &Fixture::message_header(360, 440, 0));
            assert_eq!(
                runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x20, 0, 0, 0],),
                LinuxResult::Value(expected),
            );
            let bytes = fixture.memory.inner.bytes.lock().unwrap();
            assert_eq!(&bytes[400..403], b"tru");
            assert_ne!(u32::from_le_bytes(bytes[348..352].try_into().unwrap()) & 0x20, 0,);
        }
    }
}

#[test]
fn peek_remains_observable() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 2, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.prepare_send(vec![ControlMessage::Rights(vec![0])], b"rights");
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        fixture.memory.put(360, &Fixture::iovec(400, 2));
        fixture.memory.put(300, &Fixture::message_header(360, 440, 64));
        fixture.memory.inner.fail_write.store(true, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0022, 0, 0, 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert!(fixture.descriptors.pin(2).is_err());
        fixture.memory.inner.fail_write.store(false, Ordering::Release);
        fixture.memory.put(360, &Fixture::iovec(400, 2));
        fixture.memory.put(300, &Fixture::message_header(360, 440, 64));
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x4000_0022, 0, 0, 0],),
            LinuxResult::Value(6),
        );
        assert!(fixture.descriptors.pin(2).is_ok());

        fixture.prepare_receive(0);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
            LinuxResult::Value(6),
        );
        fixture.prepare_send(Vec::new(), b"");
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(360, &Fixture::iovec(400, 0));
        fixture.memory.put(300, &Fixture::message_header(360, 440, 0));
        assert_eq!(
            runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x22, 0, 0, 0],),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn sendmmsg_error_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 2, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.put(320, b"one");
        fixture.memory.put(330, b"two!");
        fixture.memory.put(400, &Fixture::iovec(320, 3));
        fixture.memory.put(416, &Fixture::iovec(330, 4));
        fixture.memory.put(128, &Fixture::message_header(400, 0, 0));
        fixture.memory.put(192, &Fixture::message_header(416, 0, 0));
        assert_eq!(
            runtime.handle(Fixture::operation("sendmmsg"), [0, 128, 2, 0, 0, 0],),
            LinuxResult::Value(2),
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(u32::from_le_bytes(bytes[184..188].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bytes[248..252].try_into().unwrap()), 4);
        drop(bytes);

        fixture.memory.put(128, &Fixture::message_header(400, 0, 0));
        fixture.memory.put(192, &Fixture::message_header(600, 0, 0));
        assert_eq!(
            runtime.handle(Fixture::operation("sendmmsg"), [0, 128, 2, 0, 0, 0],),
            LinuxResult::Value(1),
        );
        assert_eq!(
            u32::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[184..188].try_into().unwrap(),),
            3,
        );
    }
}

#[test]
fn sendmmsg_message_commit() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 2, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("sendmmsg"), [0, 480, 2, 0, 0, 0],),
        LinuxResult::Error(Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("sendmmsg"), [0, 0, 1025, 0, 0, 0],),
        LinuxResult::Error(Errno::EINVAL),
    );
    fixture.prepare_receive(0);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0x40, 0, 0, 0],),
        LinuxResult::Error(Errno::EAGAIN),
    );
}

#[path = "batch_test.rs"]
mod batch_receive_tests;
