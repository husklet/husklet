use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hl_linux::{Errno, EventSyscalls, LinuxResult, SyscallOperation};

use super::VirtualMemory;
use super::descriptor::Set;

pub(super) mod deadline;
mod selection;
mod subscription;
mod wire;

// The C descriptor bound keeps the maximum guest import at 512 KiB.
const POLL_LIMIT: usize = 65_536;
const POLL_INVALID: i16 = 0x20;
const SIGNAL_SET_SIZE: u64 = 8;

#[derive(Clone, Copy)]
struct PollEntry {
    descriptor: i32,
    events: i16,
    returned: i16,
    guest: i32,
    generation: Option<u64>,
}

enum PollOutcome {
    Ready,
    Woken,
    TimedOut,
    Interrupted,
}

trait PollHost: Send {
    fn wait(
        &mut self,
        entries: &mut [PollEntry],
        timeout: Option<Duration>,
        cancellation: &Cancellation,
    ) -> io::Result<PollOutcome>;
}

struct LinuxPoll {
    deadlines: Arc<deadline::Queue>,
    wake: Arc<Wake>,
}

#[repr(C)]
struct NativePoll {
    descriptor: i32,
    events: i16,
    returned: i16,
}

impl PollHost for LinuxPoll {
    fn wait(
        &mut self,
        entries: &mut [PollEntry],
        timeout: Option<Duration>,
        cancellation: &Cancellation,
    ) -> io::Result<PollOutcome> {
        self.deadlines.drain();
        let mut native = entries
            .iter()
            .map(|entry| NativePoll {
                descriptor: entry.descriptor,
                events: entry.events,
                returned: 0,
            })
            .collect::<Vec<_>>();
        native.push(NativePoll {
            descriptor: cancellation.descriptor,
            events: 1,
            returned: 0,
        });
        native.push(NativePoll {
            descriptor: self.deadlines.descriptor(),
            events: 1,
            returned: 0,
        });
        native.push(NativePoll {
            descriptor: self.wake.descriptor,
            events: 1,
            returned: 0,
        });
        let timeout = timeout.map(NativeTimeout::new).transpose()?;
        let timeout_pointer = timeout.as_ref().map_or(core::ptr::null(), |value| &raw const value.0);
        // SAFETY: native is uniquely writable for its initialized length,
        // timeout is either null or a live normalized timespec, all descriptors
        // remain owned for the synchronous call, and libc retains no pointers.
        let result = unsafe { ppoll(native.as_mut_ptr(), native.len(), timeout_pointer, core::ptr::null()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        for (entry, observed) in entries.iter_mut().zip(&native) {
            entry.returned |= observed.returned;
        }
        let ready = entries.iter().any(|entry| entry.returned != 0);
        let cancellation_ready = native.get(entries.len()).is_some_and(|entry| entry.returned != 0);
        let interrupted = cancellation_ready && cancellation.drain();
        let woken = native.last().is_some_and(|entry| entry.returned != 0);
        if woken {
            self.wake.drain();
        }
        let deadline = native.get(entries.len() + 1).is_some_and(|entry| entry.returned != 0);
        if deadline {
            self.deadlines.drain();
        }
        if ready {
            Ok(PollOutcome::Ready)
        } else if interrupted {
            Ok(PollOutcome::Interrupted)
        } else if result == 0 {
            Ok(PollOutcome::TimedOut)
        } else if deadline || woken {
            Ok(PollOutcome::Woken)
        } else {
            // A host descriptor reported an event that did not map to guest
            // readiness. Treat it as an internal wake and let the typed layer
            // revalidate instead of returning a spurious zero.
            Ok(PollOutcome::Woken)
        }
    }
}

struct NativeTimeout(super::super::abi::timespec);

impl NativeTimeout {
    fn new(duration: Duration) -> io::Result<Self> {
        Ok(Self(super::super::abi::timespec {
            tv_sec: i64::try_from(duration.as_secs()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            tv_nsec: i64::from(duration.subsec_nanos()),
        }))
    }
}

pub(super) struct Cancellation {
    descriptor: i32,
    signal: AtomicI32,
    interruption: Arc<hl_sync::Interruption>,
}

impl Cancellation {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: eventfd takes scalar arguments and returns a new owned
        // descriptor; it retains no Rust storage and cannot unwind.
        let flags = super::super::abi::O_NONBLOCK | super::super::abi::O_CLOEXEC;
        let descriptor = unsafe { eventfd(0, flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            signal: AtomicI32::new(0),
            interruption: Arc::new(hl_sync::Interruption::new()),
        })
    }

    pub(super) fn request(&self, signal: i32) {
        let _ = self
            .signal
            .compare_exchange(0, signal, Ordering::AcqRel, Ordering::Acquire);
        self.wake();
    }

    pub(super) fn wake(&self) {
        self.interruption.interrupt();
        let value = 1_u64;
        // SAFETY: value is readable for eight bytes, the descriptor remains
        // owned by self, the call retains nothing, and cannot unwind.
        let _ = unsafe {
            super::super::abi::write(
                self.descriptor,
                (&raw const value).cast(),
                core::mem::size_of::<u64>(),
            )
        };
    }

    pub(super) fn drain(&self) -> bool {
        let mut value = 0_u64;
        // SAFETY: value is writable for eight bytes and the descriptor remains owned.
        let _ = unsafe {
            super::super::abi::read(
                self.descriptor,
                (&raw mut value).cast(),
                core::mem::size_of::<u64>(),
            )
        };
        self.interruption.take_pending()
    }

    pub(super) fn signal(&self) -> Option<i32> {
        let signal = self.signal.load(Ordering::Acquire);
        (signal != 0).then_some(signal)
    }

    pub(super) fn interruption(&self) -> Arc<hl_sync::Interruption> {
        Arc::clone(&self.interruption)
    }
}

impl hl_runtime::BlockingWait for Cancellation {
    fn interruption(&self) -> Arc<hl_sync::Interruption> {
        self.interruption()
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        // SAFETY: self exclusively surrenders its owned descriptor exactly
        // once; close retains no pointer and cannot unwind.
        let _ = unsafe { super::super::abi::close(self.descriptor) };
    }
}

pub(super) struct SignalMasks {
    bits: AtomicU64,
}

struct Wake {
    descriptor: i32,
}

impl Wake {
    fn new() -> io::Result<Arc<Self>> {
        let flags = super::super::abi::O_NONBLOCK | super::super::abi::O_CLOEXEC;
        // SAFETY: eventfd receives scalar values and returns one owned descriptor.
        let descriptor = unsafe { eventfd(0, flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Arc::new(Self { descriptor }))
    }

    fn drain(&self) {
        let mut value = 0_u64;
        // SAFETY: value is writable for eight bytes and the descriptor remains owned.
        let _ = unsafe {
            super::super::abi::read(
                self.descriptor,
                (&raw mut value).cast(),
                core::mem::size_of::<u64>(),
            )
        };
    }
}

impl hl_descriptor::ReadinessObserver for Wake {
    fn readiness_changed(&self) {
        let value = 1_u64;
        // SAFETY: value is readable for eight bytes and the descriptor remains owned.
        let _ = unsafe {
            super::super::abi::write(
                self.descriptor,
                (&raw const value).cast(),
                core::mem::size_of::<u64>(),
            )
        };
    }
}

impl hl_task::SignalActivityWake for Wake {
    fn signal_activity_changed(&self) {
        hl_descriptor::ReadinessObserver::readiness_changed(self);
    }

    fn process_control_activity(&self, _: hl_task::SignalActivityEvent) {
        self.signal_activity_changed();
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: self surrenders its owned descriptor exactly once.
        let _ = unsafe { super::super::abi::close(self.descriptor) };
    }
}

impl SignalMasks {
    pub(super) fn new() -> Self {
        Self {
            bits: AtomicU64::new(0),
        }
    }

    fn replace(&self, bits: u64) -> MaskScope<'_> {
        let bits = bits & !((1 << 8) | (1 << 18));
        let previous = self.bits.swap(bits, Ordering::AcqRel);
        MaskScope { masks: self, previous }
    }
}

struct MaskScope<'a> {
    masks: &'a SignalMasks,
    previous: u64,
}

impl Drop for MaskScope<'_> {
    fn drop(&mut self) {
        self.masks.bits.store(self.previous, Ordering::Release);
    }
}

pub(super) struct EventPort {
    memory: Arc<VirtualMemory>,
    descriptors: Arc<Set>,
    cancellation: Arc<Cancellation>,
    masks: Arc<SignalMasks>,
    host: Box<dyn PollHost>,
    wake: Arc<Wake>,
    tasks: Arc<hl_task::TaskRegistry>,
    thread: hl_task::ThreadId,
    _signal_activity: hl_task::SignalActivitySubscription,
}

impl EventPort {
    pub(super) fn new(
        memory: Arc<VirtualMemory>,
        descriptors: Arc<Set>,
        cancellation: Arc<Cancellation>,
        masks: Arc<SignalMasks>,
        deadlines: Arc<deadline::Queue>,
        tasks: Arc<hl_task::TaskRegistry>,
        thread: hl_task::ThreadId,
    ) -> Self {
        let wake = Wake::new().expect("readiness eventfd is available");
        let observer: Arc<dyn hl_task::SignalActivityWake> = wake.clone();
        let signal_activity = tasks.subscribe_signal_activity(observer);
        Self {
            memory,
            descriptors,
            cancellation,
            masks,
            host: Box::new(LinuxPoll {
                deadlines: Arc::clone(&deadlines),
                wake: Arc::clone(&wake),
            }),
            wake,
            tasks,
            thread,
            _signal_activity: signal_activity,
        }
    }

    fn ppoll(&mut self, arguments: [u64; 6]) -> LinuxResult {
        let entries = match self.entries(arguments[0], arguments[1]) {
            Ok(entries) => entries,
            Err(error) => return LinuxResult::Error(error),
        };
        let timeout = match self.timeout(arguments[2]) {
            Ok(timeout) => timeout,
            Err(error) => return LinuxResult::Error(error),
        };
        let mask = match self.mask(arguments[3], arguments[4]) {
            Ok(mask) => mask,
            Err(error) => return LinuxResult::Error(error),
        };
        self.poll_wait(arguments[0], entries, timeout, mask, arguments[2])
    }

    fn poll(&mut self, arguments: [u64; 6]) -> LinuxResult {
        let entries = match self.entries(arguments[0], arguments[1]) {
            Ok(entries) => entries,
            Err(error) => return LinuxResult::Error(error),
        };
        let milliseconds = arguments[2] as i32;
        let timeout = (milliseconds >= 0).then(|| Duration::from_millis(milliseconds as u64));
        self.poll_wait(arguments[0], entries, timeout, None, 0)
    }

    fn poll_wait(
        &mut self,
        address: u64,
        mut entries: Vec<PollEntry>,
        timeout: Option<Duration>,
        mask: Option<u64>,
        timeout_address: u64,
    ) -> LinuxResult {
        let started = Instant::now();
        let masks = Arc::clone(&self.masks);
        let _scope = mask.map(|bits| masks.replace(bits));
        self.revalidate(&mut entries);
        self.wake.drain();
        let mut immediate = entries.iter().any(|entry| entry.returned != 0);
        let subscriptions = if immediate {
            Vec::new()
        } else {
            match self.subscriptions(&entries) {
                Ok(subscriptions) => subscriptions,
                Err(error) => return LinuxResult::Error(error),
            }
        };
        // Close the sample/register race: a transition after the first sample
        // either appears here or wakes the registered observer.
        self.revalidate(&mut entries);
        immediate |= entries.iter().any(|entry| entry.returned != 0);
        if let Err(error) = self.wait_revalidating(&mut entries, timeout, started, immediate, mask) {
            return LinuxResult::Error(error);
        }
        let _subscriptions = subscriptions;
        let timeout_copy = (timeout_address != 0).then_some(timeout).flatten().map(|duration| {
            (
                timeout_address,
                Self::timespec(duration.saturating_sub(started.elapsed())),
            )
        });
        if self.copyout(address, &entries, timeout_copy).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(entries.iter().filter(|entry| entry.returned != 0).count() as u64)
    }

    fn wait_revalidating(
        &mut self,
        entries: &mut [PollEntry],
        timeout: Option<Duration>,
        started: Instant,
        immediate: bool,
        temporary_mask: Option<u64>,
    ) -> Result<(), Errno> {
        let mut first = true;
        loop {
            let remaining = if immediate {
                Some(Duration::ZERO)
            } else if first {
                timeout
            } else {
                timeout.map(|duration| duration.saturating_sub(started.elapsed()))
            };
            first = false;
            if !immediate && self.interrupting_signal_pending(temporary_mask) {
                return Err(Errno::EINTR);
            }
            let wait = self.host.wait(entries, remaining, &self.cancellation);
            self.revalidate(entries);
            let ready = entries.iter().any(|entry| entry.returned != 0);
            if !immediate && !ready && self.interrupting_signal_pending(temporary_mask) {
                return Err(Errno::EINTR);
            }
            match wait {
                Err(error) if error.kind() == io::ErrorKind::Interrupted && !immediate && !ready => {
                    return Err(Errno::EINTR);
                }
                Err(_) if !immediate && !ready => return Err(Errno::EIO),
                Ok(PollOutcome::Interrupted) if !immediate && !ready => return Err(Errno::EINTR),
                Ok(PollOutcome::Woken) if !ready && remaining != Some(Duration::ZERO) => {}
                Ok(PollOutcome::Ready | PollOutcome::Woken | PollOutcome::TimedOut | PollOutcome::Interrupted)
                | Err(_) => return Ok(()),
            }
        }
    }

    fn interrupting_signal_pending(&self, temporary_mask: Option<u64>) -> bool {
        let mask = temporary_mask.map(hl_task::SignalMask::from_bits);
        self.tasks.has_interrupting_signal(self.thread, mask).unwrap_or(false)
    }

    fn mask(&self, address: u64, size: u64) -> Result<Option<u64>, Errno> {
        if address == 0 {
            return Ok(None);
        }
        if size != SIGNAL_SET_SIZE {
            return Err(Errno::EINVAL);
        }
        let mut bytes = [0; 8];
        self.memory.read(address, &mut bytes).map_err(|_| Errno::EFAULT)?;
        Ok(Some(u64::from_le_bytes(bytes)))
    }

    fn revalidate(&self, entries: &mut [PollEntry]) {
        for entry in entries {
            let Some(generation) = entry.generation else {
                continue;
            };
            if !self.descriptors.current(entry.guest, generation) {
                entry.returned = POLL_INVALID;
            } else if entry.descriptor < 0 {
                entry.returned |= self.descriptors.readiness(entry.guest, entry.events).unwrap_or(0);
            }
        }
    }
}

impl EventSyscalls for EventPort {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "poll" => self.poll(arguments),
            "ppoll" => self.ppoll(arguments),
            "pselect6" => self.pselect(arguments),
            "select" => self.select(arguments),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}

unsafe extern "C" {
    fn eventfd(initial: u32, flags: i32) -> i32;
    fn ppoll(
        descriptors: *mut NativePoll,
        count: usize,
        timeout: *const super::super::abi::timespec,
        mask: *const core::ffi::c_void,
    ) -> i32;
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hl_descriptor::{
        DescriptorFlags, DescriptorTable, OpenFileDescription, Readiness, ReadinessObserver, ReadinessRegistry,
        ReadinessSubscription,
    };
    use hl_isa::GuestAddress;
    use hl_linux::SyscallFamily;
    use hl_memory::{Backing, MapRequest, MappingHost, Placement, Protection};
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

    use super::*;
    use crate::ffi::linux::MappingHostAdapter;

    const PAGE: usize = 4096;

    #[derive(Clone, Copy)]
    enum FakeOutcome {
        Complete,
        Interrupted,
    }

    #[derive(Debug)]
    struct Object {
        readiness: AtomicUsize,
        observers: ReadinessRegistry,
    }

    impl Object {
        fn new(readiness: u32) -> Self {
            Self {
                readiness: AtomicUsize::new(readiness as usize),
                observers: ReadinessRegistry::new(),
            }
        }
    }

    impl OpenFileDescription for Object {
        fn readiness(&self, interests: Readiness) -> Readiness {
            let ready = self.readiness.load(Ordering::Acquire) as u32;
            Readiness::from_bits(ready & (interests.bits() | Readiness::ERROR | Readiness::HANGUP))
        }

        fn subscribe_readiness(
            &self,
            observer: Arc<dyn ReadinessObserver>,
        ) -> Result<Box<dyn ReadinessSubscription>, hl_descriptor::ObjectError> {
            self.observers.subscribe(observer)
        }
    }

    struct Observer;
    impl ReadinessObserver for Observer {
        fn readiness_changed(&self) {}
    }

    struct FakePoll {
        calls: Arc<AtomicUsize>,
        ready: Option<(usize, i16)>,
        outcome: FakeOutcome,
        masks: Arc<SignalMasks>,
        observed_mask: Arc<Mutex<Option<u64>>>,
        observed_timeout: Arc<Mutex<Option<Option<Duration>>>>,
    }

    impl PollHost for FakePoll {
        fn wait(
            &mut self,
            entries: &mut [PollEntry],
            timeout: Option<Duration>,
            _: &Cancellation,
        ) -> io::Result<PollOutcome> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.observed_mask.lock().unwrap() = Some(self.masks.bits.load(Ordering::Acquire));
            *self.observed_timeout.lock().unwrap() = Some(timeout);
            if let Some((index, returned)) = self.ready {
                entries[index].returned = returned;
            }
            Ok(match self.outcome {
                FakeOutcome::Complete if self.ready.is_some() => PollOutcome::Ready,
                FakeOutcome::Complete => PollOutcome::TimedOut,
                FakeOutcome::Interrupted => PollOutcome::Interrupted,
            })
        }
    }

    struct ScriptPoll {
        outcomes: VecDeque<PollOutcome>,
        timeouts: Arc<Mutex<Vec<Option<Duration>>>>,
        ready_on_call: Option<(usize, Arc<Object>)>,
        calls: usize,
    }

    impl PollHost for ScriptPoll {
        fn wait(
            &mut self,
            _: &mut [PollEntry],
            timeout: Option<Duration>,
            _: &Cancellation,
        ) -> io::Result<PollOutcome> {
            self.calls += 1;
            self.timeouts.lock().unwrap().push(timeout);
            if self.ready_on_call.as_ref().is_some_and(|(call, _)| *call == self.calls) {
                let object = &self.ready_on_call.as_ref().unwrap().1;
                object.readiness.store(Readiness::READ as usize, Ordering::Release);
            }
            Ok(self.outcomes.pop_front().expect("scripted poll outcome"))
        }
    }

    struct ReusePoll {
        table: Arc<DescriptorTable>,
        descriptor: i32,
    }

    impl PollHost for ReusePoll {
        fn wait(&mut self, _: &mut [PollEntry], _: Option<Duration>, _: &Cancellation) -> io::Result<PollOutcome> {
            self.table.close(self.descriptor).unwrap();
            self.table
                .install(
                    self.descriptor,
                    Arc::new(Object::new(Readiness::READ)),
                    DescriptorFlags::default(),
                )
                .unwrap();
            Ok(PollOutcome::Woken)
        }
    }

    struct Fixture {
        memory: Arc<VirtualMemory>,
        descriptors: Arc<Set>,
        cancellation: Arc<Cancellation>,
        masks: Arc<SignalMasks>,
        tasks: Arc<TaskRegistry>,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
    }

    impl Fixture {
        fn new() -> Self {
            let memory = Arc::new(VirtualMemory::reserve(PAGE).unwrap());
            let host = MappingHostAdapter::new(Arc::clone(&memory));
            let request = MapRequest {
                placement: Placement::Fixed(GuestAddress::new(0)),
                length: PAGE as u64,
                alignment: PAGE as u64,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 1,
                    shared: false,
                },
                backing_offset: 0,
            };
            let token = host.stage_map(GuestAddress::new(0), request).unwrap();
            host.commit(&[token]).unwrap();
            let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
            let (process, thread) = tasks
                .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
                .unwrap();
            Self {
                memory,
                descriptors: Arc::new(Set::new().unwrap()),
                cancellation: Arc::new(Cancellation::new().unwrap()),
                masks: Arc::new(SignalMasks::new()),
                tasks,
                process,
                thread,
            }
        }

        fn with_object(readiness: u32) -> (Self, Arc<DescriptorTable>, Arc<Object>, i32) {
            let mut fixture = Self::new();
            let table = Arc::new(DescriptorTable::new(POLL_LIMIT as i32).unwrap());
            let streams = crate::composition::StandardStreams::new(std::io::empty(), std::io::sink(), std::io::sink());
            let descriptors = Arc::new(Set::with_table(Arc::clone(&table), &streams).unwrap());
            let object = Arc::new(Object::new(readiness));
            let installed: Arc<dyn OpenFileDescription> = object.clone();
            let descriptor = table.install(3, installed, DescriptorFlags::default()).unwrap();
            fixture.descriptors = descriptors;
            (fixture, table, object, descriptor)
        }

        fn port(
            &self,
            ready: Option<(usize, i16)>,
            outcome: FakeOutcome,
        ) -> (
            EventPort,
            Arc<AtomicUsize>,
            Arc<Mutex<Option<u64>>>,
            Arc<Mutex<Option<Option<Duration>>>>,
        ) {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed_mask = Arc::new(Mutex::new(None));
            let observed_timeout = Arc::new(Mutex::new(None));
            let host = FakePoll {
                calls: Arc::clone(&calls),
                ready,
                outcome,
                masks: Arc::clone(&self.masks),
                observed_mask: Arc::clone(&observed_mask),
                observed_timeout: Arc::clone(&observed_timeout),
            };
            let port = self.with_host(Box::new(host));
            (port, calls, observed_mask, observed_timeout)
        }

        fn with_host(&self, host: Box<dyn PollHost>) -> EventPort {
            let wake = Wake::new().unwrap();
            let observer: Arc<dyn hl_task::SignalActivityWake> = wake.clone();
            let signal_activity = self.tasks.subscribe_signal_activity(observer);
            EventPort {
                memory: Arc::clone(&self.memory),
                descriptors: Arc::clone(&self.descriptors),
                cancellation: Arc::clone(&self.cancellation),
                masks: Arc::clone(&self.masks),
                host,
                wake,
                tasks: Arc::clone(&self.tasks),
                thread: self.thread,
                _signal_activity: signal_activity,
            }
        }
    }

    fn operation() -> SyscallOperation {
        SyscallOperation {
            canonical_number: 73,
            name: "ppoll",
            family: SyscallFamily::Event,
        }
    }

    fn selection_operation() -> SyscallOperation {
        SyscallOperation {
            canonical_number: 72,
            name: "pselect6",
            family: SyscallFamily::Event,
        }
    }

    #[test]
    fn selection_timeout_mask() {
        let fixture = Fixture::new();
        fixture.memory.write(64, &1_i64.to_le_bytes()).unwrap();
        fixture.memory.write(72, &2_i64.to_le_bytes()).unwrap();
        fixture.memory.write(96, &128_u64.to_le_bytes()).unwrap();
        fixture.memory.write(104, &8_u64.to_le_bytes()).unwrap();
        fixture.memory.write(128, &u64::MAX.to_le_bytes()).unwrap();
        let (mut port, calls, mask, timeout) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(
            port.handle(selection_operation(), [0, 0, 0, 0, 64, 96]),
            LinuxResult::Value(0),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(*timeout.lock().unwrap(), Some(Some(Duration::new(1, 2))));
        assert_eq!(*mask.lock().unwrap(), Some(!((1 << 8) | (1 << 18))));
        assert_eq!(fixture.masks.bits.load(Ordering::Acquire), 0);
        let mut copied = [0_u8; 16];
        fixture.memory.read(64, &mut copied).unwrap();
        let seconds = i64::from_le_bytes(copied[..8].try_into().unwrap());
        let nanos = i64::from_le_bytes(copied[8..].try_into().unwrap());
        assert!((0..=1).contains(&seconds));
        assert!((0..1_000_000_000).contains(&nanos));
    }

    #[test]
    fn invalid_returns_immediately() {
        let fixture = Fixture::new();
        let mut records = [0_u8; 16];
        records[..4].copy_from_slice(&0_i32.to_le_bytes());
        records[4..6].copy_from_slice(&1_i16.to_le_bytes());
        records[8..12].copy_from_slice(&9_i32.to_le_bytes());
        records[12..14].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &records).unwrap();
        let (mut port, calls, _, timeout) = fixture.port(Some((0, 1)), FakeOutcome::Complete);
        assert_eq!(port.handle(operation(), [64, 2, 0, 0, 0, 0]), LinuxResult::Value(2),);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(*timeout.lock().unwrap(), Some(Some(Duration::ZERO)));
        fixture.memory.read(64, &mut records).unwrap();
        assert_eq!(i16::from_le_bytes(records[6..8].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(records[14..16].try_into().unwrap()), POLL_INVALID,);
    }

    #[test]
    fn mask_scope_restores() {
        let fixture = Fixture::new();
        fixture.memory.write(64, &2_i64.to_le_bytes()).unwrap();
        fixture.memory.write(72, &3_i64.to_le_bytes()).unwrap();
        let requested = u64::MAX;
        fixture.memory.write(96, &requested.to_le_bytes()).unwrap();
        let (mut port, calls, mask, timeout) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(port.handle(operation(), [0, 0, 64, 96, 8, 0]), LinuxResult::Value(0),);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(*mask.lock().unwrap(), Some(requested & !((1 << 8) | (1 << 18))),);
        assert_eq!(*timeout.lock().unwrap(), Some(Some(Duration::new(2, 3))),);
        assert_eq!(fixture.masks.bits.load(Ordering::Acquire), 0);
        let mut copied = [0_u8; 16];
        fixture.memory.read(64, &mut copied).unwrap();
        let seconds = i64::from_le_bytes(copied[..8].try_into().unwrap());
        let nanos = i64::from_le_bytes(copied[8..].try_into().unwrap());
        assert!((0..=2).contains(&seconds));
        assert!((0..1_000_000_000).contains(&nanos));
    }

    #[test]
    fn interruption_returns_eintr() {
        let fixture = Fixture::new();
        let mut original = [0_u8; 16];
        original[..8].copy_from_slice(&3_i64.to_le_bytes());
        fixture.memory.write(64, &original).unwrap();
        let (mut port, calls, _, _) = fixture.port(None, FakeOutcome::Interrupted);
        assert_eq!(
            port.handle(operation(), [0, 0, 64, 0, 0, 0]),
            LinuxResult::Error(Errno::EINTR),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let mut copied = [0_u8; 16];
        fixture.memory.read(64, &mut copied).unwrap();
        assert_eq!(copied, original);
    }

    #[test]
    fn pending_handler_interrupts_before_host_wait() {
        let fixture = Fixture::new();
        let signal = hl_task::SignalNumber::new(10).unwrap();
        fixture
            .tasks
            .set_action(
                fixture.process,
                signal,
                hl_task::SignalAction {
                    disposition: hl_task::SignalDisposition::Handler(0x1000),
                    ..hl_task::SignalAction::DEFAULT
                },
            )
            .unwrap();
        fixture
            .tasks
            .enqueue_signal(
                hl_task::PendingTarget::Thread(fixture.thread),
                hl_task::SignalInfo::bare(signal),
            )
            .unwrap();
        let (mut port, calls, _, _) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(
            port.handle(operation(), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINTR),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn finite_internal_wake_reblocks_for_remaining_timeout() {
        let fixture = Fixture::new();
        fixture.memory.write(64, &0_i64.to_le_bytes()).unwrap();
        fixture.memory.write(72, &100_000_000_i64.to_le_bytes()).unwrap();
        let timeouts = Arc::new(Mutex::new(Vec::new()));
        let host = ScriptPoll {
            outcomes: VecDeque::from([PollOutcome::Woken, PollOutcome::TimedOut]),
            timeouts: Arc::clone(&timeouts),
            ready_on_call: None,
            calls: 0,
        };
        let mut port = fixture.with_host(Box::new(host));
        assert_eq!(port.handle(operation(), [0, 0, 64, 0, 0, 0]), LinuxResult::Value(0));
        let observed = timeouts.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0], Some(Duration::from_millis(100)));
        assert!(observed[1].is_some_and(|remaining| remaining <= Duration::from_millis(100)));
    }

    #[test]
    fn infinite_internal_wake_reblocks_until_guest_readiness() {
        let (fixture, _, object, descriptor) = Fixture::with_object(0);
        let mut record = [0_u8; 8];
        record[..4].copy_from_slice(&descriptor.to_le_bytes());
        record[4..6].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &record).unwrap();
        let timeouts = Arc::new(Mutex::new(Vec::new()));
        let host = ScriptPoll {
            outcomes: VecDeque::from([PollOutcome::Woken, PollOutcome::Woken]),
            timeouts: Arc::clone(&timeouts),
            ready_on_call: Some((2, object)),
            calls: 0,
        };
        let mut port = fixture.with_host(Box::new(host));
        assert_eq!(port.handle(operation(), [64, 1, 0, 0, 0, 0]), LinuxResult::Value(1));
        assert_eq!(*timeouts.lock().unwrap(), vec![None, None]);
    }

    #[test]
    fn readiness_precedes_interrupt() {
        let fixture = Fixture::new();
        let mut record = [0_u8; 8];
        record[4..6].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &record).unwrap();
        let (mut port, calls, _, _) = fixture.port(Some((0, 1)), FakeOutcome::Interrupted);
        assert_eq!(port.handle(operation(), [64, 1, 0, 0, 0, 0]), LinuxResult::Value(1),);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancellation_wakes() {
        let fixture = Fixture::new();
        let cancellation = Arc::clone(&fixture.cancellation);
        let masks = Arc::clone(&fixture.masks);
        let descriptors = Arc::clone(&fixture.descriptors);
        let memory = Arc::clone(&fixture.memory);
        let tasks = Arc::clone(&fixture.tasks);
        let task_thread = fixture.thread;
        let (started, admission) = std::sync::mpsc::channel();
        let (completed, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let deadlines = super::deadline::Queue::new().unwrap();
            let mut port = EventPort::new(memory, descriptors, cancellation, masks, deadlines, tasks, task_thread);
            started.send(()).unwrap();
            completed.send(port.handle(operation(), [0, 0, 0, 0, 0, 0])).unwrap();
        });
        admission.recv().unwrap();
        fixture.cancellation.request(2);
        assert_eq!(
            result.recv_timeout(Duration::from_secs(1)).unwrap(),
            LinuxResult::Error(Errno::EINTR),
        );
        thread.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_futex() {
        let cancellation = Cancellation::new().unwrap();
        let interruption = cancellation.interruption();
        assert!(!interruption.is_pending());
        cancellation.request(2);
        assert!(interruption.take_pending());
    }

    #[test]
    fn cancellation_wake_is_consumed_by_one_wait() {
        let cancellation = Cancellation::new().unwrap();
        let mut host = LinuxPoll {
            deadlines: super::deadline::Queue::new().unwrap(),
            wake: Wake::new().unwrap(),
        };
        cancellation.wake();
        assert!(matches!(
            host.wait(&mut [], Some(Duration::ZERO), &cancellation).unwrap(),
            PollOutcome::Interrupted
        ));
        assert!(matches!(
            host.wait(&mut [], Some(Duration::ZERO), &cancellation).unwrap(),
            PollOutcome::TimedOut
        ));
    }

    #[test]
    fn faults_before_waiting() {
        let fixture = Fixture::new();
        let (mut port, calls, _, _) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(
            port.handle(operation(), [PAGE as u64, 1, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn timeout_fault_precedence() {
        let fixture = Fixture::new();
        let (mut port, calls, _, _) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(
            port.handle(operation(), [0, 0, PAGE as u64, 0, 0, 0]),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        fixture.memory.write(64, &0_i64.to_le_bytes()).unwrap();
        fixture.memory.write(72, &1_000_000_000_i64.to_le_bytes()).unwrap();
        assert_eq!(
            port.handle(selection_operation(), [0, 0, 0, 0, 64, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn mixed_wake() {
        let (fixture, _, _, descriptor) = Fixture::with_object(Readiness::READ);
        let mut records = [0_u8; 16];
        records[..4].copy_from_slice(&0_i32.to_le_bytes());
        records[4..6].copy_from_slice(&1_i16.to_le_bytes());
        records[8..12].copy_from_slice(&descriptor.to_le_bytes());
        records[12..14].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &records).unwrap();
        let (mut port, _, _, timeout) = fixture.port(Some((0, 1)), FakeOutcome::Complete);
        assert_eq!(port.handle(operation(), [64, 2, 0, 0, 0, 0]), LinuxResult::Value(2));
        assert_eq!(*timeout.lock().unwrap(), Some(Some(Duration::ZERO)));
    }

    #[test]
    fn observer_capacity() {
        let (fixture, _, object, descriptor) = Fixture::with_object(0);
        let observer: Arc<dyn ReadinessObserver> = Arc::new(Observer);
        let held = (0..64)
            .map(|_| object.observers.subscribe(observer.clone()).unwrap())
            .collect::<Vec<_>>();
        let mut record = [0_u8; 8];
        record[..4].copy_from_slice(&descriptor.to_le_bytes());
        record[4..6].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &record).unwrap();
        let (mut port, calls, _, _) = fixture.port(None, FakeOutcome::Complete);
        assert_eq!(
            port.handle(operation(), [64, 1, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::ENFILE)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(held);
    }

    #[test]
    fn reuse_invalidates() {
        let (fixture, table, _, descriptor) = Fixture::with_object(0);
        let mut record = [0_u8; 8];
        record[..4].copy_from_slice(&descriptor.to_le_bytes());
        record[4..6].copy_from_slice(&1_i16.to_le_bytes());
        fixture.memory.write(64, &record).unwrap();
        let mut port = fixture.with_host(Box::new(ReusePoll { table, descriptor }));
        assert_eq!(port.handle(operation(), [64, 1, 0, 0, 0, 0]), LinuxResult::Value(1),);
        fixture.memory.read(64, &mut record).unwrap();
        assert_eq!(i16::from_le_bytes(record[6..].try_into().unwrap()), POLL_INVALID);
    }
}
