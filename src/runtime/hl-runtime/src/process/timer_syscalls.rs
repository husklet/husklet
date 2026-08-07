//! POSIX timer syscalls layered over the per-process timer registry.

use hl_linux::{ClockIdentity, Errno, GuestMarshaller, GuestMemory, LinuxResult, TimeFutexAbi, TimerEvent, TimerPlan};
use hl_time::Timespec;

use crate::RuntimeProcessSyscalls;

use super::{Timer, TimerRegistry, absolute_deadline};

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
        if let Some(staged) = staged
            && let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture))
        {
            return LinuxResult::Error(error.errno());
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

    pub(super) fn timer_value(timer: Timer, now: u64) -> Timespec {
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
                .map(hl_time::MonotonicInstant::nanoseconds)
                .map(Some)?,
        };
        value.ok_or(Errno::EOVERFLOW)
    }
}
