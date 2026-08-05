use std::sync::{Arc, Mutex, Weak};

use hl_linux::{ClockIdentity, Errno, GuestMarshaller, GuestMemory, LinuxResult, TimeFutexAbi, TimerEvent, TimerPlan};
use hl_task::{PendingTarget, ProcessId, SignalInfo, SignalNumber};
use hl_time::Timespec;

use crate::{AlarmRegistry, RuntimeProcessSyscalls};

const TIMER_LIMIT: usize = 64;

fn absolute_deadline(now: u64, clock_now: u64, requested: u64) -> (u64, u64) {
    if requested >= clock_now {
        let translated = now.saturating_add(requested - clock_now);
        (translated, translated)
    } else {
        (now, now.saturating_sub(clock_now - requested))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Timer {
    clock: ClockIdentity,
    event: Option<TimerEvent>,
    deadline: Option<u64>,
    first: u64,
    interval: u64,
    reported: u64,
    generation: u64,
    token: Option<u64>,
}

#[cfg(test)]
mod test {
    use super::absolute_deadline;

    #[test]
    fn absolute_anchor_translation() {
        assert_eq!(absolute_deadline(20, 100, 130), (50, 50));
        assert_eq!(absolute_deadline(20, 100, 85), (20, 5));
    }
}

/// Bounded process-owned POSIX timer catalog.
///
/// Expiry uses the shared bounded alarm scheduler and queues tagged Linux
/// signal metadata through the task registry; it never creates per-timer host
/// threads or raises host signals into guest execution.
pub struct TimerRegistry {
    slots: Mutex<[Option<Timer>; TIMER_LIMIT]>,
    delivery: Option<TimerDelivery>,
}

struct TimerDelivery {
    process: ProcessId,
    alarms: Arc<AlarmRegistry>,
}

impl Default for TimerRegistry {
    fn default() -> Self {
        Self {
            slots: Mutex::new([None; TIMER_LIMIT]),
            delivery: None,
        }
    }
}

impl TimerRegistry {
    #[must_use]
    pub fn new(process: ProcessId, alarms: Arc<AlarmRegistry>) -> Arc<Self> {
        Arc::new(Self {
            slots: Mutex::new([None; TIMER_LIMIT]),
            delivery: Some(TimerDelivery { process, alarms }),
        })
    }

    fn allocate(&self, clock: ClockIdentity, event: Option<TimerEvent>) -> Option<usize> {
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = slots.iter().position(Option::is_none)?;
        slots[id] = Some(Timer {
            clock,
            event,
            deadline: None,
            first: 0,
            interval: 0,
            reported: 0,
            generation: 0,
            token: None,
        });
        Some(id)
    }

    fn timer(&self, id: i32) -> Option<Timer> {
        usize::try_from(id).ok().and_then(|id| {
            self.slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .copied()
                .flatten()
        })
    }

    fn scheduler_now(&self) -> Result<u64, Errno> {
        self.delivery
            .as_ref()
            .map(|delivery| delivery.alarms.schedule_callback_now())
            .ok_or(Errno::ENOSYS)
    }

    fn replace(&self, id: i32, expected: Timer, timer: Timer) -> bool {
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = slots.get_mut(id) else {
            return false;
        };
        if *slot != Some(expected) {
            return false;
        }
        *slot = Some(timer);
        true
    }

    fn remove(&self, id: i32) -> bool {
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        let removed = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(id)
            .and_then(Option::take);
        if let (Some(timer), Some(delivery)) = (removed, &self.delivery) {
            if let Some(token) = timer.token {
                delivery.alarms.cancel_callback(token);
            }
            self.remove_pending(id as usize, timer, delivery);
        }
        removed.is_some()
    }

    pub fn clear(&self) {
        let old = {
            let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *slots, [None; TIMER_LIMIT])
        };
        if let Some(delivery) = &self.delivery {
            for (id, timer) in old.into_iter().enumerate() {
                let Some(timer) = timer else { continue };
                if let Some(token) = timer.token {
                    delivery.alarms.cancel_callback(token);
                }
                self.remove_pending(id, timer, delivery);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn allocate_for_test(&self, clock: ClockIdentity) -> Option<usize> {
        self.allocate(clock, None)
    }

    #[cfg(test)]
    pub(crate) fn allocated_for_test(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flatten()
            .count()
    }

    fn arm(self: &Arc<Self>, id: usize, generation: u64, deadline: u64) -> Result<(), ()> {
        let Some(delivery) = &self.delivery else { return Ok(()) };
        let weak = Arc::downgrade(self);
        let callback = Arc::new(move || Self::expire(&weak, id, generation));
        let token = delivery.alarms.schedule_callback(deadline, callback)?;
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(timer) = slots.get_mut(id).and_then(Option::as_mut) else {
            delivery.alarms.cancel_callback(token);
            return Ok(());
        };
        if timer.generation != generation || timer.deadline != Some(deadline) {
            delivery.alarms.cancel_callback(token);
        } else {
            timer.token = Some(token);
        }
        Ok(())
    }

    fn replace_scheduled(self: &Arc<Self>, id: i32, current: Timer, mut replacement: Timer) -> Result<bool, ()> {
        let token = if let (Some(deadline), Some(delivery)) = (replacement.deadline, &self.delivery) {
            let weak = Arc::downgrade(self);
            let generation = replacement.generation;
            let index = usize::try_from(id).map_err(|_| ())?;
            let callback = Arc::new(move || Self::expire(&weak, index, generation));
            Some(delivery.alarms.schedule_callback(deadline, callback)?)
        } else {
            None
        };
        replacement.token = token;
        if !self.replace(id, current, replacement) {
            if let (Some(token), Some(delivery)) = (token, &self.delivery) {
                delivery.alarms.cancel_callback(token);
            }
            return Ok(false);
        }
        if let (Some(token), Some(delivery)) = (current.token, &self.delivery) {
            delivery.alarms.cancel_callback(token);
        }
        Ok(true)
    }

    fn target(&self, event: TimerEvent, delivery: &TimerDelivery) -> PendingTarget {
        event
            .target_thread
            .and_then(|tid| u32::try_from(tid).ok())
            .and_then(|tid| delivery.alarms.timer_thread(delivery.process, tid))
            .map_or(PendingTarget::Process(delivery.process), PendingTarget::Thread)
    }

    fn remove_pending(&self, id: usize, timer: Timer, delivery: &TimerDelivery) {
        let event = timer.event.unwrap_or(TimerEvent {
            value: id as u64,
            signal: 14,
            notification: 0,
            target_thread: None,
        });
        if event.notification == 1 {
            return;
        }
        if let Ok(signal) = SignalNumber::new(event.signal as u8) {
            delivery
                .alarms
                .remove_timer_signal(self.target(event, delivery), signal, (id + 1) as u32);
        }
    }

    fn expire(registry: &Weak<Self>, id: usize, generation: u64) {
        let Some(registry) = registry.upgrade() else { return };
        let Some(delivery) = &registry.delivery else { return };
        let now = delivery.alarms.schedule_callback_now();
        let next = {
            let mut slots = registry.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(timer) = slots.get_mut(id).and_then(Option::as_mut) else {
                return;
            };
            if timer.generation != generation {
                return;
            }
            timer.token = None;
            let next = if timer.interval == 0 {
                None
            } else {
                let prior = timer.deadline.unwrap_or(now);
                let periods = now.saturating_sub(prior) / timer.interval + 1;
                Some(prior.saturating_add(periods.saturating_mul(timer.interval)))
            };
            timer.deadline = next;
            let event = timer.event.unwrap_or(TimerEvent {
                value: id as u64,
                signal: 14,
                notification: 0,
                target_thread: None,
            });
            // Keep the timer slot locked through queue publication. Delete
            // takes this lock before removing a tagged pending instance, so
            // an expiry cannot publish after timer_delete has completed.
            if event.notification != 1 {
                if let Ok(signal) = SignalNumber::new(event.signal as u8) {
                    let target = registry.target(event, delivery);
                    delivery.alarms.deliver_timer_signal(
                        target,
                        SignalInfo {
                            signal,
                            code: -2,
                            error: 0,
                            sender_process: 0,
                            sender_user: 0,
                            value: event.value,
                            address: 0,
                            source_tag: (id + 1) as u32,
                        },
                    );
                }
            }
            next
        };
        if let Some(next) = next {
            if registry.arm(id, generation, next).is_err() {
                let mut slots = registry.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(timer) = slots.get_mut(id).and_then(Option::as_mut) {
                    if timer.generation == generation && timer.deadline == Some(next) {
                        timer.deadline = None;
                        timer.interval = 0;
                    }
                }
            }
        }
    }
}

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub fn with_timers(mut self, timers: std::sync::Arc<TimerRegistry>) -> Self {
        self.timers = Some(timers);
        self
    }

    pub(crate) fn timer_create(&self, clock: i32, event: u64, output: u64) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let TimerPlan::Create { clock, event, output } = (match abi.timer_create(clock, event, output) {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(error.errno()),
        }) else {
            unreachable!()
        };
        if matches!(event, Some(TimerEvent { notification: 2, .. })) {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let Some(timers) = &self.timers else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        // Probe the result pointer before reserving an id, so a guest fault
        // cannot transiently consume registry capacity.
        match abi.stage_timer_id(output, 0) {
            Ok(_) => {}
            Err(error) => return LinuxResult::Error(error.errno()),
        }
        let Some(id) = timers.allocate(clock, event) else {
            return LinuxResult::Error(Errno::EAGAIN);
        };
        let staged = match abi.stage_timer_id(output, id as i32) {
            Ok(staged) => staged,
            Err(error) => {
                timers.remove(id as i32);
                return LinuxResult::Error(error.errno());
            }
        };
        if let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            timers.remove(id as i32);
            return LinuxResult::Error(error.errno());
        }
        LinuxResult::Value(0)
    }

    pub(crate) fn timer_settime(&self, id: i32, flags: u32, source: u64, old: u64) -> LinuxResult {
        let Some(timers) = &self.timers else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Some(current) = timers.timer(id) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let TimerPlan::Set {
            absolute,
            value,
            interval,
            ..
        } = (match abi.timer_set(id, flags, source, old) {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(error.errno()),
        })
        else {
            unreachable!()
        };
        let now = match timers.scheduler_now() {
            Ok(now) => now,
            Err(error) => return LinuxResult::Error(error),
        };
        let old_value = Self::timer_value(current, now);
        let staged = if old == 0 {
            None
        } else {
            match abi.stage_timer(old, Timespec::from_nanoseconds(current.interval), old_value) {
                Ok(staged) => Some(staged),
                Err(error) => return LinuxResult::Error(error.errno()),
            }
        };
        let Some(value_ns) = value.checked_nanoseconds() else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Some(interval_ns) = interval.checked_nanoseconds() else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let mut replacement = current;
        replacement.token = None;
        replacement.generation = replacement.generation.wrapping_add(1);
        if value_ns == 0 {
            replacement.deadline = None;
            replacement.interval = 0;
        } else {
            let (deadline, first) = if absolute {
                let clock_now = match self.timer_now(current.clock) {
                    Ok(clock_now) => clock_now,
                    Err(error) => return LinuxResult::Error(error),
                };
                absolute_deadline(now, clock_now, value_ns)
            } else {
                let deadline = now.saturating_add(value_ns);
                (deadline, deadline)
            };
            replacement.deadline = Some(deadline);
            replacement.first = first;
            replacement.interval = interval_ns;
            replacement.reported = 0;
        }
        if let Some(staged) = staged {
            if let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
                return LinuxResult::Error(error.errno());
            }
        }
        match timers.replace_scheduled(id, current, replacement) {
            Ok(true) => {}
            Ok(false) => return LinuxResult::Error(Errno::EINVAL),
            Err(()) => return LinuxResult::Error(Errno::EAGAIN),
        }
        LinuxResult::Value(0)
    }

    pub(crate) fn timer_gettime(&self, id: i32, output: u64) -> LinuxResult {
        let Some(timers) = &self.timers else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Some(timer) = timers.timer(id) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let now = match timers.scheduler_now() {
            Ok(now) => now,
            Err(error) => return LinuxResult::Error(error),
        };
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let staged = match abi.stage_timer(
            output,
            Timespec::from_nanoseconds(timer.interval),
            Self::timer_value(timer, now),
        ) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn timer_getoverrun(&self, id: i32) -> LinuxResult {
        let Some(timers) = &self.timers else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Some(mut timer) = timers.timer(id) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let current = timer;
        let now = match timers.scheduler_now() {
            Ok(now) => now,
            Err(error) => return LinuxResult::Error(error),
        };
        let elapsed = if timer.interval == 0 || now < timer.first {
            0
        } else {
            (now - timer.first) / timer.interval + 1
        };
        let overrun = elapsed
            .saturating_sub(timer.reported)
            .saturating_sub(1)
            .min(i32::MAX as u64);
        timer.reported = elapsed;
        timers.replace(id, current, timer);
        LinuxResult::Value(overrun)
    }

    pub(crate) fn timer_delete(&self, id: i32) -> LinuxResult {
        if self.timers.as_ref().is_some_and(|timers| timers.remove(id)) {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EINVAL)
        }
    }

    fn timer_value(timer: Timer, now: u64) -> Timespec {
        let remaining = timer.deadline.map_or(0, |deadline| {
            if deadline > now {
                deadline - now
            } else if timer.interval > 0 {
                timer.interval - ((now - deadline) % timer.interval)
            } else {
                0
            }
        });
        Timespec::from_nanoseconds(remaining)
    }

    fn timer_now(&self, clock: ClockIdentity) -> Result<u64, Errno> {
        let provider = self.clock.as_ref().ok_or(Errno::ENOSYS)?;
        let value = match clock {
            ClockIdentity::Realtime
            | ClockIdentity::RealtimeCoarse
            | ClockIdentity::RealtimeAlarm
            | ClockIdentity::Tai => provider.realtime_now().map_err(|_| Errno::EIO)?.checked_nanoseconds(),
            ClockIdentity::ProcessCpu => self
                .cpu_clock
                .as_ref()
                .ok_or(Errno::ENOSYS)?
                .aggregate()
                .map_err(|_| Errno::EIO)?
                .checked_nanoseconds(),
            ClockIdentity::ThreadCpu => self
                .cpu_clock
                .as_ref()
                .ok_or(Errno::ENOSYS)?
                .current()
                .map_err(|_| Errno::EIO)?
                .checked_nanoseconds(),
            _ => provider
                .monotonic_now()
                .map_err(|_| Errno::EIO)
                .map(|value| value.nanoseconds())
                .map(Some)?,
        };
        value.ok_or(Errno::EOVERFLOW)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ClockIdentity, TIMER_LIMIT, Timer, TimerRegistry};
    use crate::{AlarmRegistry, AlarmScheduler};
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

    #[test]
    fn registry_admits_sixty_four_and_reuses_deleted_ids() {
        let registry = TimerRegistry::default();
        for id in 0..TIMER_LIMIT {
            assert_eq!(registry.allocate(ClockIdentity::Monotonic, None), Some(id));
        }
        assert_eq!(registry.allocate(ClockIdentity::Monotonic, None), None);
        assert!(registry.remove(17));
        assert_eq!(registry.allocate(ClockIdentity::Realtime, None), Some(17));
    }

    #[test]
    fn periodic_remaining_folds_from_the_original_deadline() {
        let timer = Timer {
            clock: ClockIdentity::Monotonic,
            deadline: Some(100),
            first: 100,
            interval: 30,
            reported: 0,
            event: None,
            generation: 0,
            token: None,
        };
        assert_eq!(
            super::RuntimeProcessSyscalls::<TestMemory>::timer_value(timer, 99).checked_nanoseconds(),
            Some(1)
        );
        assert_eq!(
            super::RuntimeProcessSyscalls::<TestMemory>::timer_value(timer, 100).checked_nanoseconds(),
            Some(30)
        );
        assert_eq!(
            super::RuntimeProcessSyscalls::<TestMemory>::timer_value(timer, 171).checked_nanoseconds(),
            Some(19)
        );
    }

    #[test]
    fn scheduler_failure_preserves_previous_arm() {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(0, 0, &[], 65_536).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::default()).unwrap();
        let alarms = AlarmRegistry::new(tasks, Arc::new(FailingScheduler));
        let registry = TimerRegistry::new(process, alarms);
        let id = registry.allocate(ClockIdentity::Monotonic, None).unwrap() as i32;
        let current = registry.timer(id).unwrap();
        let mut replacement = current;
        replacement.deadline = Some(50);
        replacement.generation = 1;

        assert_eq!(registry.replace_scheduled(id, current, replacement), Err(()));
        assert_eq!(registry.timer(id), Some(current));
    }

    struct FailingScheduler;

    impl AlarmScheduler for FailingScheduler {
        fn now(&self) -> u64 {
            0
        }

        fn schedule(&self, _: u64, _: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()> {
            Err(())
        }

        fn cancel(&self, _: u64) {}
    }

    struct TestMemory;

    impl hl_linux::GuestMemory for TestMemory {
        fn probe(&self, _: u64, length: usize, _: hl_linux::GuestAccess) -> Result<usize, hl_linux::GuestFault> {
            Ok(length)
        }
        fn read(&self, _: u64, output: &mut [u8]) -> Result<usize, hl_linux::GuestFault> {
            Ok(output.len())
        }
        fn write(&self, _: u64, input: &[u8]) -> Result<usize, hl_linux::GuestFault> {
            Ok(input.len())
        }
    }
}
