use super::*;

#[derive(Debug)]
struct SocketWait {
    interruption: Arc<hl_sync::Interruption>,
    outcomes: Mutex<VecDeque<hl_sync::WaitOutcome>>,
    now: u64,
}

impl crate::SocketWait for SocketWait {
    fn interruption(&self) -> Arc<hl_sync::Interruption> {
        self.interruption.clone()
    }
    fn monotonic_now(&self) -> Result<hl_time::MonotonicInstant, hl_time::ClockError> {
        Ok(hl_time::MonotonicInstant::from_nanoseconds(self.now))
    }
    fn wait(
        &self,
        _: &hl_sync::WaitQueue,
        _: u64,
        _: Option<hl_time::Deadline>,
    ) -> Result<hl_sync::WaitOutcome, hl_sync::WaitError> {
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(hl_sync::WaitOutcome::Interrupted))
    }
}

#[test]
fn waitforone_work_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("socketpair"), [1, 2, 0, 32, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.prepare_send(Vec::new(), b"one");
        assert_eq!(
            runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
            LinuxResult::Value(3),
        );
        fixture.memory.put(300, &Fixture::iovec(400, 8));
        fixture.memory.put(316, &Fixture::iovec(420, 8));
        fixture.memory.put(64, &Fixture::message_header(300, 0, 0));
        fixture.memory.put(128, &Fixture::message_header(316, 0, 0));
        assert_eq!(
            runtime.handle(Fixture::operation("recvmmsg"), [1, 64, 2, 0x1_0000, 0, 0],),
            LinuxResult::Value(1),
        );
        let bytes = fixture.memory.inner.bytes.lock().unwrap();
        assert_eq!(u32::from_le_bytes(bytes[120..124].try_into().unwrap()), 3);
        assert_eq!(&bytes[400..403], b"one");
        drop(bytes);
        fixture.memory.put(480, &[0; 16]);
        assert_eq!(
            runtime.handle(Fixture::operation("recvmmsg"), [1, 64, 1, 0, 480, 0],),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn interrupt_linux_ordering() {
    let fixture = Fixture::new();
    let wait = Arc::new(SocketWait {
        interruption: Arc::new(hl_sync::Interruption::new()),
        outcomes: Mutex::new(VecDeque::from([
            hl_sync::WaitOutcome::Interrupted,
            hl_sync::WaitOutcome::TimedOut,
        ])),
        now: 10,
    });
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64).with_wait_port(wait);
    assert_eq!(
        runtime.handle(Fixture::operation("socketpair"), [1, 2, 0, 32, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.memory.put(300, &Fixture::iovec(400, 8));
    fixture.memory.put(64, &Fixture::message_header(300, 0, 0));
    fixture
        .memory
        .put(480, &[0_i64.to_le_bytes(), 100_i64.to_le_bytes()].concat());
    assert_eq!(
        runtime.handle(Fixture::operation("recvmmsg"), [1, 64, 1, 0, 480, 0],),
        LinuxResult::Error(Errno::EINTR),
    );
    assert_eq!(
        i64::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[488..496].try_into().unwrap(),),
        100,
    );
    assert_eq!(
        runtime.handle(Fixture::operation("recvmmsg"), [1, 64, 1, 0, 480, 0],),
        LinuxResult::Value(0),
    );
    fixture.prepare_send(Vec::new(), b"safe");
    assert_eq!(
        runtime.handle(Fixture::operation("sendmsg"), [0, 128, 0, 0, 0, 0]),
        LinuxResult::Value(4),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("recvmmsg"), [1, 480, 2, 0x40, 0, 0],),
        LinuxResult::Error(Errno::EFAULT),
    );
    fixture.prepare_receive(0);
    assert_eq!(
        runtime.handle(Fixture::operation("recvmsg"), [1, 300, 0, 0, 0, 0]),
        LinuxResult::Value(4),
    );
}
