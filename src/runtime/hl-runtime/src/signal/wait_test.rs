use super::*;

#[test]
fn masks_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let mask = (1_u64 << 8) | (1_u64 << 18) | (1_u64 << 9);
        fixture.memory.put(16, &mask.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigprocmask"), [2, 16, 32, 8, 0, 0]),
            LinuxResult::Value(0)
        );
        let state = fixture.tasks.deliver_thread_state(fixture.thread).unwrap();
        assert_eq!(state.mask.bits(), 1_u64 << 9);
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigprocmask"), [2, 16, 252, 8, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            fixture.tasks.deliver_thread_state(fixture.thread).unwrap().mask,
            state.mask
        );
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigaction"), [9, 240, 0, 7, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigtimedwait"), [252, 0, 0, 8, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
    }
}

#[test]
fn action_query_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigaction"), [10, 0, 32, 8, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(&fixture.memory.0.lock().unwrap()[32..64], &[0; 32]);

        let handler = 0x1234_u64;
        fixture.memory.put(80, &handler.to_le_bytes());
        fixture.memory.put(88, &0_u64.to_le_bytes());
        fixture.memory.put(96, &0_u64.to_le_bytes());
        fixture.memory.put(104, &0_u64.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigaction"), [10, 80, 0, 8, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigaction"), [10, 0, 128, 8, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(&fixture.memory.0.lock().unwrap()[128..136], &handler.to_le_bytes());
        for signal in [9, 19] {
            assert_eq!(
                runtime.handle(Fixture::operation("rt_sigaction"), [signal, 0, 160, 8, 0, 0]),
                LinuxResult::Error(hl_linux::Errno::EINVAL)
            );
        }
    }
}

#[test]
fn sigtimedwait_exact_retry() {
    let fixture = Fixture::new();
    let signal = hl_task::SignalNumber::new(35).unwrap();
    fixture
        .tasks
        .enqueue_signal(
            hl_task::PendingTarget::Process(fixture.process),
            hl_task::SignalInfo {
                value: 77,
                ..hl_task::SignalInfo::bare(signal)
            },
        )
        .unwrap();
    fixture.memory.put(16, &(1_u64 << 34).to_le_bytes());
    fixture.memory.put(64, &[0; 16]);
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigtimedwait"), [16, 252, 64, 8, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EFAULT)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigtimedwait"), [16, 128, 64, 8, 0, 0]),
        LinuxResult::Value(35)
    );
    assert_eq!(
        u64::from_le_bytes(fixture.memory.0.lock().unwrap()[152..160].try_into().unwrap()),
        77
    );
}

#[test]
fn signalfd_until_drop() {
    let fixture = Fixture::new();
    let signal = hl_task::SignalNumber::new(35).unwrap();
    fixture
        .tasks
        .enqueue_signal(
            hl_task::PendingTarget::Process(fixture.process),
            hl_task::SignalInfo {
                value: 91,
                ..hl_task::SignalInfo::bare(signal)
            },
        )
        .unwrap();
    fixture.memory.put(16, &(1_u64 << 34).to_le_bytes());
    fixture.memory.put(64, &[0; 16]);
    let queue = TaskSignalQueue::new(fixture.tasks.clone(), fixture.thread);
    let reserved = queue.prepare(EventSignalMask::from_bits(1_u64 << 34)).unwrap().unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigtimedwait"), [16, 128, 64, 8, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EAGAIN)
    );
    drop(reserved);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigtimedwait"), [16, 128, 64, 8, 0, 0]),
        LinuxResult::Value(35)
    );
}

#[test]
fn sigtimedwait_once_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let signal = hl_task::SignalNumber::new(35).unwrap();
        fixture.memory.put(16, &(1_u64 << 34).to_le_bytes());
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let blocked =
            std::thread::spawn(move || runtime.handle(Fixture::operation("rt_sigtimedwait"), [16, 128, 0, 8, 0, 0]));
        fixture
            .tasks
            .enqueue_signal(
                hl_task::PendingTarget::Process(fixture.process),
                hl_task::SignalInfo::bare(signal),
            )
            .unwrap();
        assert_eq!(blocked.join().unwrap(), LinuxResult::Value(35));
        assert!(
            !fixture
                .tasks
                .has_signal_wait(fixture.thread, hl_task::SignalMask::from_bits(1_u64 << 34))
                .unwrap()
        );
    }
}

#[test]
fn sigsuspend_signal_pending() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let old_mask = hl_task::SignalMask::from_bits(1_u64 << 34);
        fixture.tasks.set_signal_mask(fixture.thread, old_mask).unwrap();
        fixture.memory.put(16, &0_u64.to_le_bytes());
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let blocked =
            std::thread::spawn(move || runtime.handle(Fixture::operation("rt_sigsuspend"), [16, 8, 0, 0, 0, 0]));
        let signal = hl_task::SignalNumber::new(35).unwrap();
        fixture
            .tasks
            .enqueue_signal(
                hl_task::PendingTarget::Thread(fixture.thread),
                hl_task::SignalInfo::bare(signal),
            )
            .unwrap();
        assert_eq!(blocked.join().unwrap(), LinuxResult::Error(hl_linux::Errno::EINTR));
        assert_eq!(
            fixture.tasks.deliver_thread_state(fixture.thread).unwrap().mask,
            old_mask
        );
        assert!(
            fixture
                .tasks
                .has_signal_wait(fixture.thread, hl_task::SignalMask::from_bits(1_u64 << 34))
                .unwrap()
        );
        let first = fixture.tasks.prepare_forced_delivery(fixture.thread).unwrap();
        assert_eq!(first.info().signal, signal);
        drop(first);
        let retry = fixture.tasks.prepare_forced_delivery(fixture.thread).unwrap();
        assert_eq!(retry.info().signal, signal);
        drop(retry);
        let runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.deliver_signal_boundary().unwrap(),
            crate::SignalBoundaryOutcome::Terminate {
                signal: signal.get(),
                dumped_core: false,
            }
        );
    }
}

#[test]
fn sigsuspend_changing_mask() {
    let fixture = Fixture::new();
    let old_mask = hl_task::SignalMask::from_bits(1_u64 << 10);
    fixture.tasks.set_signal_mask(fixture.thread, old_mask).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigsuspend"), [252, 4, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigsuspend"), [252, 8, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EFAULT)
    );
    assert_eq!(
        fixture.tasks.deliver_thread_state(fixture.thread).unwrap().mask,
        old_mask
    );
}

#[test]
fn x86_pending_delivery() {
    let fixture = Fixture::new();
    let signal = hl_task::SignalNumber::new(12).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64, fixture.thread);
    let blocked = std::thread::spawn(move || runtime.handle(Fixture::operation("pause"), [0; 6]));
    fixture
        .tasks
        .enqueue_signal(
            hl_task::PendingTarget::Process(fixture.process),
            hl_task::SignalInfo::bare(signal),
        )
        .unwrap();
    assert_eq!(blocked.join().unwrap(), LinuxResult::Error(hl_linux::Errno::EINTR),);
    assert_eq!(
        fixture.tasks.dequeue_signal(fixture.thread).unwrap().unwrap().0.signal,
        signal,
    );
}

#[test]
fn sigsuspend_realtime_fifo() {
    let fixture = Fixture::new();
    let signal = hl_task::SignalNumber::new(35).unwrap();
    fixture.memory.put(16, &0_u64.to_le_bytes());
    for value in [1, 2] {
        fixture
            .tasks
            .enqueue_signal(
                hl_task::PendingTarget::Process(fixture.process),
                hl_task::SignalInfo {
                    value,
                    ..hl_task::SignalInfo::bare(signal)
                },
            )
            .unwrap();
    }
    for expected in [1, 2] {
        let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("rt_sigsuspend"), [16, 8, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINTR),
        );
        let forced = fixture.tasks.prepare_forced_delivery(fixture.thread).unwrap();
        assert_eq!(forced.info().value, expected);
        fixture.tasks.commit_forced_delivery(forced).unwrap().unwrap();
    }
}
