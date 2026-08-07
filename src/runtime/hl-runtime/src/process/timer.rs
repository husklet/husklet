use std::sync::{Arc, Mutex, Weak};

use hl_linux::{ClockIdentity, Errno, TimerEvent};
use hl_task::{PendingTarget, ProcessId, SignalInfo, SignalNumber};

use crate::AlarmRegistry;
#[cfg(test)]
use crate::RuntimeProcessSyscalls;

#[path = "timer_syscalls.rs"]
mod syscalls;

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
        let Some(delivery) = &self.delivery else {
            return;
        };
        for (id, timer) in old.into_iter().enumerate() {
            let Some(timer) = timer else { continue };
            if let Some(token) = timer.token {
                delivery.alarms.cancel_callback(token);
            }
            self.remove_pending(id, timer, delivery);
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
        if let Some(next) = next
            && registry.arm(id, generation, next).is_err()
        {
            let mut slots = registry.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(timer) = slots.get_mut(id).and_then(Option::as_mut)
                && timer.generation == generation
                && timer.deadline == Some(next)
            {
                timer.deadline = None;
                timer.interval = 0;
            }
        }
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
