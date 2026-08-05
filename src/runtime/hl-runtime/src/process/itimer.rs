use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_linux::{Errno, GuestMarshaller, GuestMemory, IntervalTimer, LinuxResult, TimeFutexAbi};
use hl_sync::Interruption;
use hl_task::{PendingTarget, ProcessId, SignalInfo, SignalNumber, TaskRegistry};

use crate::RuntimeProcessSyscalls;

pub trait AlarmScheduler: Send + Sync {
    fn now(&self) -> u64;
    fn schedule(&self, deadline: u64, callback: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()>;
    fn cancel(&self, token: u64);
}

#[derive(Clone, Copy, Default)]
struct TimerState {
    generation: u64,
    deadline: Option<u64>,
    interval: u64,
    token: Option<u64>,
}

/// Lock-free view of the CPU interval timers for one process identity.
///
/// Readers retain this process-lifetime object across execution admission. PID
/// slot reuse installs a different object, so a late reader cannot observe the
/// replacement process. An odd generation denotes an update in progress; a
/// reader that races repeated updates fails closed by reporting an armed timer.
pub struct CpuTimerPublication {
    generation: AtomicU64,
    virtual_armed: AtomicBool,
    virtual_deadline: AtomicU64,
    prof_armed: AtomicBool,
    prof_deadline: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuTimerSnapshot {
    pub generation: u64,
    pub virtual_deadline: Option<u64>,
    pub prof_deadline: Option<u64>,
}

impl CpuTimerPublication {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            virtual_armed: AtomicBool::new(false),
            virtual_deadline: AtomicU64::new(0),
            prof_armed: AtomicBool::new(false),
            prof_deadline: AtomicU64::new(0),
        }
    }

    fn publish(&self, virtual_deadline: Option<u64>, prof_deadline: Option<u64>) {
        let generation = self.generation.load(Ordering::Relaxed);
        if generation >= u64::MAX - 1 {
            // Exhaustion is terminal and fail-closed: no old continuation can
            // become current again after generation wraparound.
            self.generation.store(u64::MAX, Ordering::Release);
            return;
        }
        self.generation.store(generation + 1, Ordering::SeqCst);
        self.virtual_deadline
            .store(virtual_deadline.unwrap_or(0), Ordering::Relaxed);
        self.virtual_armed.store(virtual_deadline.is_some(), Ordering::Relaxed);
        self.prof_deadline.store(prof_deadline.unwrap_or(0), Ordering::Relaxed);
        self.prof_armed.store(prof_deadline.is_some(), Ordering::Relaxed);
        self.generation.store(generation + 2, Ordering::Release);
    }

    #[must_use]
    pub fn snapshot(&self) -> CpuTimerSnapshot {
        for _ in 0..3 {
            let before = self.generation.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let virtual_armed = self.virtual_armed.load(Ordering::Relaxed);
            let virtual_deadline = self.virtual_deadline.load(Ordering::Relaxed);
            let prof_armed = self.prof_armed.load(Ordering::Relaxed);
            let prof_deadline = self.prof_deadline.load(Ordering::Relaxed);
            let after = self.generation.load(Ordering::Acquire);
            if before == after {
                return CpuTimerSnapshot {
                    generation: after,
                    virtual_deadline: virtual_armed.then_some(virtual_deadline),
                    prof_deadline: prof_armed.then_some(prof_deadline),
                };
            }
        }
        // A native continuation must decline when publication is unstable.
        CpuTimerSnapshot {
            generation: self.generation.load(Ordering::Acquire),
            virtual_deadline: Some(0),
            prof_deadline: Some(0),
        }
    }
}

#[derive(Default)]
struct ProcessCpuTimers {
    virtual_timer: TimerState,
    prof_timer: TimerState,
    publication: Option<Arc<CpuTimerPublication>>,
}

pub struct AlarmRegistry {
    tasks: Arc<TaskRegistry>,
    scheduler: Arc<dyn AlarmScheduler>,
    timers: Mutex<BTreeMap<ProcessId, TimerState>>,
    cpu_timers: Mutex<BTreeMap<ProcessId, ProcessCpuTimers>>,
    interruptions: Mutex<BTreeMap<hl_task::ThreadId, Weak<Interruption>>>,
}

impl AlarmRegistry {
    #[must_use]
    pub fn new(tasks: Arc<TaskRegistry>, scheduler: Arc<dyn AlarmScheduler>) -> Arc<Self> {
        Arc::new(Self {
            tasks,
            scheduler,
            timers: Mutex::new(BTreeMap::new()),
            cpu_timers: Mutex::new(BTreeMap::new()),
            interruptions: Mutex::new(BTreeMap::new()),
        })
    }

    /// Registers the cancellation edge used by interruptible blocking syscalls.
    /// Signal delivery wakes the host waiter before the scheduler builds the
    /// guest signal frame, matching Linux's `EINTR` boundary.
    pub fn register_interruption(&self, thread: hl_task::ThreadId, interruption: Arc<Interruption>) {
        self.interruptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread, Arc::downgrade(&interruption));
    }

    fn current(&self, process: ProcessId) -> IntervalTimer {
        let now = self.scheduler.now();
        let state = self
            .timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&process)
            .copied()
            .unwrap_or_default();
        IntervalTimer {
            interval: Self::timespec(state.interval),
            value: Self::timespec(state.deadline.map_or(0, |deadline| deadline.saturating_sub(now))),
        }
    }

    fn current_cpu(&self, process: ProcessId, which: i32, now: u64) -> IntervalTimer {
        let state = self
            .cpu_timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&process)
            .map(|timers| {
                if which == 1 {
                    timers.virtual_timer
                } else {
                    timers.prof_timer
                }
            })
            .unwrap_or_default();
        IntervalTimer {
            interval: Self::timespec(state.interval),
            value: Self::timespec(state.deadline.map_or(0, |deadline| deadline.saturating_sub(now))),
        }
    }

    fn replace_cpu(&self, process: ProcessId, which: i32, now: u64, timer: IntervalTimer) -> Result<IntervalTimer, ()> {
        let previous = self.current_cpu(process, which, now);
        let value = timer.value.checked_nanoseconds().ok_or(())?;
        let interval = timer.interval.checked_nanoseconds().ok_or(())?;
        let mut timers = self
            .cpu_timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let timers = timers.entry(process).or_default();
        let state = if which == 1 {
            &mut timers.virtual_timer
        } else {
            &mut timers.prof_timer
        };
        state.interval = interval;
        state.deadline = (value != 0).then(|| now.saturating_add(value));
        Self::publish_cpu(timers);
        Ok(previous)
    }

    /// Returns a process-generation-qualified handle suitable for retention by
    /// an admitted executor. Only this lookup takes the registry lock.
    pub fn cpu_timer_publication(&self, process: ProcessId) -> Arc<CpuTimerPublication> {
        let mut timers = self
            .cpu_timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let timers = timers.entry(process).or_default();
        if let Some(publication) = &timers.publication {
            return Arc::clone(publication);
        }
        let publication = Arc::new(CpuTimerPublication::new());
        publication.publish(timers.virtual_timer.deadline, timers.prof_timer.deadline);
        timers.publication = Some(Arc::clone(&publication));
        publication
    }

    fn publish_cpu(timers: &ProcessCpuTimers) {
        if let Some(publication) = &timers.publication {
            publication.publish(timers.virtual_timer.deadline, timers.prof_timer.deadline);
        }
    }

    pub(crate) fn poll_cpu(&self, process: ProcessId, virtual_now: u64, prof_now: u64) {
        let mut signals = Vec::new();
        {
            let mut timers = self
                .cpu_timers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(timers) = timers.get_mut(&process) else { return };
            for which in [1, 2] {
                let now = if which == 1 { virtual_now } else { prof_now };
                let state = if which == 1 {
                    &mut timers.virtual_timer
                } else {
                    &mut timers.prof_timer
                };
                let Some(deadline) = state.deadline else { continue };
                if now < deadline {
                    continue;
                }
                state.deadline = if state.interval == 0 {
                    None
                } else {
                    let periods = now.saturating_sub(deadline) / state.interval + 1;
                    Some(deadline.saturating_add(periods.saturating_mul(state.interval)))
                };
                signals.push(if which == 1 { 26 } else { 27 });
            }
            Self::publish_cpu(timers);
        }
        for raw in signals {
            if let Ok(signal) = SignalNumber::new(raw) {
                let _ = self
                    .tasks
                    .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal));
            }
        }
    }

    pub fn remove(&self, process: ProcessId) {
        let state = self
            .timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&process);
        if let Some(token) = state.and_then(|state| state.token) {
            self.scheduler.cancel(token);
        }
        if let Some(timers) = self
            .cpu_timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&process)
        {
            if let Some(publication) = timers.publication {
                publication.publish(None, None);
            }
        }
    }

    fn replace(self: &Arc<Self>, process: ProcessId, timer: IntervalTimer) -> Result<IntervalTimer, ()> {
        let previous = self.current(process);
        let value = timer.value.checked_nanoseconds().ok_or(())?;
        let interval = timer.interval.checked_nanoseconds().ok_or(())?;
        let now = self.scheduler.now();
        let mut timers = self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = timers.entry(process).or_default();
        if let Some(token) = state.token.take() {
            self.scheduler.cancel(token);
        }
        state.generation = state.generation.wrapping_add(1);
        state.interval = interval;
        state.deadline = (value != 0).then(|| now.saturating_add(value));
        let generation = state.generation;
        let deadline = state.deadline;
        drop(timers);
        if let Some(deadline) = deadline {
            self.arm(process, generation, deadline)?;
        }
        Ok(previous)
    }

    fn arm(self: &Arc<Self>, process: ProcessId, generation: u64, deadline: u64) -> Result<(), ()> {
        let weak = Arc::downgrade(self);
        let callback = Arc::new(move || Self::expire(&weak, process, generation));
        let token = self.scheduler.schedule(deadline, callback)?;
        let mut timers = self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = timers.get_mut(&process).ok_or(())?;
        if state.generation != generation {
            self.scheduler.cancel(token);
            return Ok(());
        }
        state.token = Some(token);
        Ok(())
    }

    fn expire(registry: &Weak<Self>, process: ProcessId, generation: u64) {
        let Some(registry) = registry.upgrade() else { return };
        let now = registry.scheduler.now();
        let next = {
            let mut timers = registry
                .timers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(state) = timers.get_mut(&process) else { return };
            if state.generation != generation {
                return;
            }
            state.token = None;
            let next = if state.interval == 0 {
                None
            } else {
                let prior = state.deadline.unwrap_or(now);
                let periods = now.saturating_sub(prior) / state.interval + 1;
                Some(prior.saturating_add(periods.saturating_mul(state.interval)))
            };
            state.deadline = next;
            next
        };
        if let Ok(signal) = SignalNumber::new(14) {
            let _ = registry
                .tasks
                .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal));
            registry.interrupt_process(process);
        }
        if let Some(deadline) = next {
            let _ = registry.arm(process, generation, deadline);
        }
    }

    fn interrupt_process(&self, process: ProcessId) {
        let threads = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == process)
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        let mut interruptions = self
            .interruptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        interruptions.retain(|_, value| value.strong_count() != 0);
        for thread in threads {
            if let Some(interruption) = interruptions.get(&thread).and_then(Weak::upgrade) {
                interruption.interrupt();
            }
        }
    }

    pub(crate) fn schedule_callback(&self, deadline: u64, callback: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()> {
        self.scheduler.schedule(deadline, callback)
    }

    pub(crate) fn schedule_callback_now(&self) -> u64 {
        self.scheduler.now()
    }

    pub(crate) fn timer_thread(&self, process: ProcessId, number: u32) -> Option<hl_task::ThreadId> {
        self.tasks
            .snapshot()
            .threads
            .into_iter()
            .find(|thread| thread.process == process && thread.id.number() == number)
            .map(|thread| thread.id)
    }

    pub(crate) fn cancel_callback(&self, token: u64) {
        self.scheduler.cancel(token);
    }

    pub(crate) fn deliver_timer_signal(&self, target: PendingTarget, info: SignalInfo) {
        let process = match target {
            PendingTarget::Process(process) => process,
            PendingTarget::Thread(thread) => {
                let Some(thread) = self
                    .tasks
                    .snapshot()
                    .threads
                    .into_iter()
                    .find(|candidate| candidate.id == thread)
                else {
                    return;
                };
                thread.process
            }
        };
        let _ = self.tasks.enqueue_source_signal(target, info);
        self.interrupt_process(process);
    }

    pub(crate) fn remove_timer_signal(&self, target: PendingTarget, signal: SignalNumber, source_tag: u32) {
        let _ = self.tasks.remove_source_signal(target, signal, source_tag);
    }

    fn timespec(nanoseconds: u64) -> hl_time::Timespec {
        hl_time::Timespec::new(nanoseconds / 1_000_000_000, (nanoseconds % 1_000_000_000) as u32).unwrap_or_default()
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub fn with_alarms(mut self, alarms: Arc<AlarmRegistry>) -> Self {
        self.alarms = Some(alarms);
        self
    }

    pub(crate) fn getitimer(&self, which: i32, output: u64) -> LinuxResult {
        if !(0..=2).contains(&which) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let timer = match (&self.alarms, which) {
            (Some(alarms), 0) => alarms.current(self.process),
            (Some(alarms), 1 | 2) => match self.cpu_now(which) {
                Ok(now) => alarms.current_cpu(self.process, which, now),
                Err(error) => return LinuxResult::Error(error),
            },
            (None, _) => IntervalTimer::default(),
            _ => unreachable!(),
        };
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let staged = match abi.stage_interval(output, timer) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn alarm(&self, seconds: u32) -> LinuxResult {
        let Some(alarms) = &self.alarms else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let timer = IntervalTimer {
            interval: hl_time::Timespec::default(),
            value: hl_time::Timespec::new(u64::from(seconds), 0).unwrap_or_default(),
        };
        let previous = match alarms.replace(self.process, timer) {
            Ok(previous) => previous,
            Err(()) => return LinuxResult::Error(Errno::EAGAIN),
        };
        let nanoseconds = previous.value.checked_nanoseconds().unwrap_or(u64::MAX);
        LinuxResult::Value(nanoseconds.div_ceil(1_000_000_000))
    }

    pub(crate) fn setitimer(&self, which: i32, source: u64, old: u64) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let timer = match abi.interval_timer(source) {
            Ok(timer) => timer,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if !(0..=2).contains(&which) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Some(alarms) = &self.alarms else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let replaced = if which == 0 {
            alarms.replace(self.process, timer)
        } else {
            match self.cpu_now(which) {
                Ok(now) => alarms.replace_cpu(self.process, which, now, timer),
                Err(error) => return LinuxResult::Error(error),
            }
        };
        let previous = match replaced {
            Ok(previous) => previous,
            Err(()) => return LinuxResult::Error(Errno::EAGAIN),
        };
        if old == 0 {
            return LinuxResult::Value(0);
        }
        let staged = match abi.stage_interval(old, previous) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn poll_cpu_itimers(&self) {
        let (Some(alarms), Ok(virtual_now), Ok(prof_now)) = (&self.alarms, self.cpu_now(1), self.cpu_now(2)) else {
            return;
        };
        alarms.poll_cpu(self.process, virtual_now, prof_now);
    }

    fn cpu_now(&self, which: i32) -> Result<u64, Errno> {
        let clock = self.cpu_clock.as_ref().ok_or(Errno::ENOSYS)?;
        let value = if which == 1 { clock.user() } else { clock.aggregate() };
        value
            .map_err(|_| Errno::EIO)?
            .checked_nanoseconds()
            .ok_or(Errno::EOVERFLOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, SignalMask};

    struct Scheduler;

    impl AlarmScheduler for Scheduler {
        fn now(&self) -> u64 {
            0
        }

        fn schedule(&self, _: u64, _: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()> {
            Ok(1)
        }

        fn cancel(&self, _: u64) {}
    }

    #[test]
    fn cpu_timer_fires_only_after_cpu_deadline() {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(0, 0, &[], 65_536).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::default()).unwrap();
        let alarms = AlarmRegistry::new(Arc::clone(&tasks), Arc::new(Scheduler));
        let timer = IntervalTimer {
            interval: hl_time::Timespec::default(),
            value: hl_time::Timespec::new(0, 20_000_000).unwrap(),
        };
        alarms.replace_cpu(process, 1, 100_000_000, timer).unwrap();
        alarms.poll_cpu(process, 119_999_999, 119_999_999);
        assert!(!tasks.has_deliverable_except(thread, SignalMask::from_bits(0)).unwrap());
        alarms.poll_cpu(process, 120_000_000, 120_000_000);
        assert!(tasks.has_deliverable_except(thread, SignalMask::from_bits(0)).unwrap());
        assert_eq!(
            alarms.current_cpu(process, 1, 120_000_000).value,
            hl_time::Timespec::default()
        );
    }

    #[test]
    fn cpu_timer_publication_tracks_set_expiry_and_exit() {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(0, 0, &[], 65_536).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::default()).unwrap();
        let alarms = AlarmRegistry::new(tasks, Arc::new(Scheduler));
        let publication = alarms.cpu_timer_publication(process);
        let initial = publication.snapshot();
        assert_eq!(initial.virtual_deadline, None);

        let timer = IntervalTimer {
            interval: hl_time::Timespec::default(),
            value: hl_time::Timespec::new(0, 20_000_000).unwrap(),
        };
        alarms.replace_cpu(process, 1, 100_000_000, timer).unwrap();
        let armed = publication.snapshot();
        assert_eq!(armed.virtual_deadline, Some(120_000_000));
        assert!(armed.generation > initial.generation);

        alarms.poll_cpu(process, 120_000_000, 0);
        let expired = publication.snapshot();
        assert_eq!(expired.virtual_deadline, None);
        assert!(expired.generation > armed.generation);

        alarms.replace_cpu(process, 2, 10, timer).unwrap();
        alarms.remove(process);
        let retired = publication.snapshot();
        assert_eq!(retired.virtual_deadline, None);
        assert_eq!(retired.prof_deadline, None);
        let replacement = alarms.cpu_timer_publication(process);
        assert!(!Arc::ptr_eq(&publication, &replacement));
    }

    #[test]
    fn cpu_timer_publication_includes_already_armed_timers() {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(0, 0, &[], 65_536).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::default()).unwrap();
        let alarms = AlarmRegistry::new(tasks, Arc::new(Scheduler));
        let timer = IntervalTimer {
            interval: hl_time::Timespec::default(),
            value: hl_time::Timespec::new(0, 20_000_000).unwrap(),
        };
        alarms.replace_cpu(process, 1, 100_000_000, timer).unwrap();

        let publication = alarms.cpu_timer_publication(process);

        assert_eq!(publication.snapshot().virtual_deadline, Some(120_000_000));
    }
}
