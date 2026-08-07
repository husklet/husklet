//! Interval timer syscalls layered over the per-process alarm registry.

use std::sync::Arc;

use hl_linux::{Errno, GuestMarshaller, GuestMemory, IntervalTimer, LinuxResult, TimeFutexAbi};

use crate::RuntimeProcessSyscalls;

use super::AlarmRegistry;

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
        let Ok(previous) = alarms.replace(self.process, timer) else {
            return LinuxResult::Error(Errno::EAGAIN);
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
        let Ok(previous) = replaced else {
            return LinuxResult::Error(Errno::EAGAIN);
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
