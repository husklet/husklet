use super::*;
use crate::{OperationError, SourceError, TaskSignalQueue};
use hl_event::{
    EventResourceKey, InotifyMask, TimerClockSource, WatchBinding, WatchNodeIdentity, WatchPathIdentity, WatchRequest,
    WatchSource, WatchSourceError, WatchSourceObserver, WatchSourceSubscription,
};
use hl_linux::{DescriptorIoSyscalls, GuestAccess, GuestFault, SyscallFamily};
use hl_task::{
    PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalInfo, SignalNumber, TaskRegistry,
};
use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct TestMemory {
    inner: Arc<TestMemoryInner>,
}

#[derive(Debug)]
struct TestMemoryInner {
    bytes: Mutex<Vec<u8>>,
    fault_write: AtomicBool,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            inner: Arc::new(TestMemoryInner {
                bytes: Mutex::new(vec![0; size]),
                fault_write: AtomicBool::new(false),
            }),
        }
    }

    fn store(&self, address: usize, bytes: &[u8]) {
        self.inner.bytes.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }
}

impl Clone for TestMemory {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl GuestMemory for TestMemory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let available = self.inner.bytes.lock().unwrap().len().saturating_sub(address as usize);
        if available < length {
            return Err(GuestFault { address, access });
        }
        Ok(length)
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        output.copy_from_slice(&self.inner.bytes.lock().unwrap()[start..start + output.len()]);
        Ok(output.len())
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.inner.fault_write.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.store(address as usize, input);
        Ok(input.len())
    }
}

#[derive(Debug, Default)]
struct ManualClock {
    monotonic: AtomicU64,
    realtime: AtomicU64,
}

impl MonotonicClock for ManualClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(
            self.monotonic.load(Ordering::Acquire),
        ))
    }
}

impl RealtimeClock for ManualClock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::from_nanoseconds(self.realtime.load(Ordering::Acquire)))
    }
}

impl TimerClockSource for ManualClock {
    fn realtime_generation(&self) -> u64 {
        0
    }
    fn schedule_callback(
        &self,
        _deadline_nanoseconds: u64,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<u64, ClockError> {
        callback();
        Ok(1)
    }
}

#[derive(Debug)]
struct TimerSource(Arc<ManualClock>);

impl TimerEventSource for TimerSource {
    fn clock(&self) -> Result<(EventResourceKey, Arc<dyn TimerClockSource>), SourceError> {
        Ok((EventResourceKey::new(1).unwrap(), self.0.clone()))
    }
}

#[derive(Debug)]
struct SignalSource(Arc<TaskSignalQueue>);

impl SignalEventSource for SignalSource {
    fn queue(&self) -> Result<(EventResourceKey, Arc<dyn hl_event::SignalQueue>), SourceError> {
        Ok((EventResourceKey::new(2).unwrap(), self.0.clone()))
    }
}

#[derive(Default)]
struct WatchState {
    observer: Option<Arc<dyn WatchSourceObserver>>,
    watches: BTreeMap<u64, InotifyMask>,
}

#[derive(Debug, Default)]
struct WatchFixture {
    state: Mutex<WatchState>,
}

impl std::fmt::Debug for WatchState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WatchState")
            .field("watches", &self.watches)
            .finish()
    }
}

struct WatchSubscription;

impl WatchSourceSubscription for WatchSubscription {
    fn quiesce(&self) {}
}

impl WatchSource for WatchFixture {
    fn resolve(&self, request: WatchRequest<'_>) -> Result<WatchBinding, WatchSourceError> {
        if request.path != b"/watched" {
            return Err(WatchSourceError::NotFound);
        }
        Ok(WatchBinding {
            node: WatchNodeIdentity { device: 1, object: 2 },
            path: WatchPathIdentity(3),
            is_directory: false,
        })
    }

    fn add(&self, _binding: WatchBinding, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        self.state.lock().unwrap().watches.insert(token, mask);
        Ok(())
    }

    fn modify(&self, token: u64, mask: InotifyMask) -> Result<(), WatchSourceError> {
        self.state.lock().unwrap().watches.insert(token, mask);
        Ok(())
    }

    fn remove(&self, token: u64) -> Result<(), WatchSourceError> {
        self.state
            .lock()
            .unwrap()
            .watches
            .remove(&token)
            .map(|_| ())
            .ok_or(WatchSourceError::NotFound)
    }

    fn subscribe(
        &self,
        observer: Arc<dyn WatchSourceObserver>,
    ) -> Result<Box<dyn WatchSourceSubscription>, WatchSourceError> {
        self.state.lock().unwrap().observer = Some(observer);
        Ok(Box::new(WatchSubscription))
    }
}

#[derive(Debug)]
struct WatchProvider(Arc<WatchFixture>);

impl WatchEventSource for WatchProvider {
    fn watches(&self) -> Result<(EventResourceKey, Arc<dyn WatchSource>), SourceError> {
        Ok((EventResourceKey::new(3).unwrap(), self.0.clone()))
    }
}

struct Fixture {
    table: Arc<DescriptorTable>,
    catalog: Arc<EventCatalog>,
    operations: Arc<OperationRegistry>,
    memory: TestMemory,
    timer: Arc<ManualClock>,
    signals: Arc<TaskSignalQueue>,
    process: hl_task::ProcessId,
    watch: Arc<WatchFixture>,
}

impl Fixture {
    fn new() -> Self {
        let tasks = Arc::new(
            TaskRegistry::new(RegistryConfig {
                max_processes: 2,
                max_threads: 2,
                max_groups: 2,
                max_pending_signals: 8,
                online_cpus: 1,
            })
            .unwrap(),
        );
        let (process, thread) = tasks
            .create_init(
                ProcessCredentials::new(1000, 1000, &[], 2).unwrap(),
                ProcessLimits::empty(),
            )
            .unwrap();
        Self {
            table: Arc::new(DescriptorTable::new(8).unwrap()),
            catalog: Arc::new(EventCatalog::new(8).unwrap()),
            operations: Arc::new(OperationRegistry::new()),
            memory: TestMemory::new(512),
            timer: Arc::new(ManualClock::default()),
            signals: Arc::new(TaskSignalQueue::new(tasks, thread)),
            process,
            watch: Arc::new(WatchFixture::default()),
        }
    }

    fn runtime(&self, architecture: GuestArchitecture) -> RuntimeEventSyscalls<TestMemory> {
        RuntimeEventSyscalls::new(
            self.table.clone(),
            self.catalog.clone(),
            self.memory.clone(),
            architecture,
        )
        .with_event_operations(self.operations.clone())
        .with_event_sources(
            Arc::new(TimerSource(self.timer.clone())),
            Arc::new(SignalSource(self.signals.clone())),
            Arc::new(WatchProvider(self.watch.clone())),
        )
    }

    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::Event,
        }
    }

    fn catalog_objects(&self) -> usize {
        self.catalog.freeze_checkpoint();
        let count = self.catalog.checkpoint_image().unwrap().objects.len();
        self.catalog.thaw_checkpoint();
        count
    }

    fn fd(result: LinuxResult) -> u64 {
        let LinuxResult::Value(value) = result else {
            panic!("{result:?}");
        };
        value
    }
}

#[test]
fn timer_final_close() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture);
        let timer = Fixture::fd(runtime.handle(Fixture::operation("timerfd_create"), [1, 0, 0, 0, 0, 0]));
        let identity = fixture.table.pin(timer as i32).unwrap().description_identity();
        assert!(fixture.operations.timer(identity).is_ok());
        fixture.memory.store(32, &[0; 32]);
        fixture.memory.store(48, &1_i64.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("timerfd_settime"), [timer, 0, 32, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        fixture.memory.inner.fault_write.store(true, Ordering::Release);
        fixture.memory.store(72, &[0; 32]);
        fixture.memory.store(88, &2_i64.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("timerfd_settime"), [timer, 0, 72, 128, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );
        fixture.memory.inner.fault_write.store(false, Ordering::Release);
        assert_eq!(
            runtime.handle(Fixture::operation("timerfd_gettime"), [timer, 160, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i64::from_le_bytes(fixture.memory.inner.bytes.lock().unwrap()[176..184].try_into().unwrap()),
            1
        );
        fixture.table.close(timer as i32).unwrap();
        assert_eq!(
            fixture.operations.timer(identity).unwrap_err(),
            OperationError::NotFound
        );
        assert_eq!(fixture.catalog_objects(), 0);
        assert_eq!(
            runtime.handle(Fixture::operation("timerfd_create"), [1, 0, 0, 0, 0, 0]),
            LinuxResult::Value(timer),
        );
    }
}

#[test]
fn signalfd_validation_close() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64);
    fixture.memory.store(8, &(1_u64 << 9).to_le_bytes());
    let signal = Fixture::fd(runtime.handle(Fixture::operation("signalfd4"), [u64::MAX, 8, 8, 0x80800, 0, 0]));
    let identity = fixture.table.pin(signal as i32).unwrap().description_identity();
    assert!(fixture.table.flags(signal as i32).unwrap().closes_on_exec());
    let mut info = SignalInfo::bare(SignalNumber::new(10).unwrap());
    info.value = 99;
    fixture
        .signals
        .enqueue(PendingTarget::Process(fixture.process), info)
        .unwrap();
    let mut filesystem = crate::RuntimeFilesystemSyscalls::new(
        fixture.table.clone(),
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    );
    fixture.memory.inner.fault_write.store(true, Ordering::Release);
    assert_eq!(
        filesystem.handle(Fixture::operation("read"), [signal, 256, 128, 0, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    fixture.memory.inner.fault_write.store(false, Ordering::Release);
    assert_eq!(
        filesystem.handle(Fixture::operation("read"), [signal, 256, 128, 0, 0, 0]),
        LinuxResult::Value(128),
    );
    fixture.memory.store(8, &(1_u64 << 11).to_le_bytes());
    assert_eq!(
        runtime.handle(Fixture::operation("signalfd4"), [signal, 8, 8, 0, 0, 0]),
        LinuxResult::Value(signal),
    );
    let event = Fixture::fd(runtime.handle(Fixture::operation("eventfd2"), [0; 6]));
    assert_eq!(
        runtime.handle(Fixture::operation("signalfd4"), [event, 8, 8, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("signalfd4"), [u64::MAX, 1, 8, u32::MAX as u64, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    fixture.table.close(signal as i32).unwrap();
    assert_eq!(
        fixture.operations.signal(identity).unwrap_err(),
        OperationError::NotFound
    );
    assert_eq!(fixture.catalog_objects(), 1);
}

#[test]
fn inotify_final_close() {
    let fixture = Fixture::new();
    fixture.memory.store(16, b"/watched\0");
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64);
    let notify = Fixture::fd(runtime.handle(Fixture::operation("inotify_init1"), [0x80800, 0, 0, 0, 0, 0]));
    let identity = fixture.table.pin(notify as i32).unwrap().description_identity();
    let watch = Fixture::fd(runtime.handle(
        Fixture::operation("inotify_add_watch"),
        [notify, 16, InotifyMask::CREATE as u64, 0, 0, 0],
    ));
    assert_eq!(
        runtime.handle(Fixture::operation("inotify_rm_watch"), [notify, watch, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    let mut filesystem =
        crate::RuntimeFilesystemSyscalls::new(fixture.table.clone(), fixture.memory.clone(), GuestArchitecture::X86_64);
    fixture.memory.inner.fault_write.store(true, Ordering::Release);
    assert_eq!(
        filesystem.handle(Fixture::operation("read"), [notify, 256, 64, 0, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    fixture.memory.inner.fault_write.store(false, Ordering::Release);
    assert_eq!(
        filesystem.handle(Fixture::operation("read"), [notify, 256, 64, 0, 0, 0]),
        LinuxResult::Value(16),
    );
    fixture.table.close(notify as i32).unwrap();
    assert_eq!(
        fixture.operations.watch(identity).unwrap_err(),
        OperationError::NotFound
    );
    assert_eq!(fixture.catalog_objects(), 0);
    assert_eq!(
        runtime.handle(Fixture::operation("inotify_init1"), [0, 0, 0, 0, 0, 0]),
        LinuxResult::Value(notify),
    );
}

#[test]
fn absent_report_enosys() {
    let mut runtime = RuntimeEventSyscalls::new(
        Arc::new(DescriptorTable::new(4).unwrap()),
        Arc::new(EventCatalog::new(4).unwrap()),
        TestMemory::new(64),
        GuestArchitecture::Aarch64,
    );
    for (name, arguments) in [
        ("timerfd_create", [1, 0, 0, 0, 0, 0]),
        ("signalfd4", [u64::MAX, 1, 8, 0, 0, 0]),
        ("inotify_init1", [0; 6]),
    ] {
        assert_eq!(
            runtime.handle(Fixture::operation(name), arguments),
            LinuxResult::Error(Errno::ENOSYS),
        );
    }
}
