use hl_linux::{ClockIdentity, Errno, GuestMarshaller, GuestMemory, LinuxResult, ProcessAbi, TimeFutexAbi};
use hl_sync::Interruption;
use hl_time::Timespec;
use std::sync::Arc;

use crate::RuntimeProcessSyscalls;

struct SleepWake {
    tasks: Arc<hl_task::TaskRegistry>,
    thread: hl_task::ThreadId,
    interruption: Arc<Interruption>,
}

impl hl_task::SignalActivityWake for SleepWake {
    fn signal_activity_changed(&self) {
        if self
            .tasks
            .has_deliverable_except(self.thread, hl_task::SignalMask::from_bits(0))
            .unwrap_or(false)
        {
            self.interruption.interrupt();
        }
    }
}

#[path = "time_ports.rs"]
mod ports;

pub use ports::{
    CpuClockPort, ResourceUsageScope, RobustExitPort, RuntimeFutexPort, RuntimeSleepOutcome, RuntimeSleepPort,
};

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn getrusage(&self, who: i32, destination: u64) -> LinuxResult {
        let scope = match who {
            0 | 1 => ResourceUsageScope::Process,
            -1 => ResourceUsageScope::Children,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let Some(clock) = &self.cpu_clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Ok(usage) = clock.resource_usage(self.process, scope) else {
            return LinuxResult::Error(Errno::EIO);
        };
        let staged = match ProcessAbi::new(&self.memory, self.architecture).stage_usage(destination, usage) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn set_robust_list(&self, head: u64, length: u64) -> LinuxResult {
        let plan = match TimeFutexAbi::new(&self.memory, self.architecture).robust_list(head, length as usize) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match self
            .tasks
            .set_robust_list(self.thread, hl_task::RobustListRegistration::new(plan.head))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn get_robust_list(&self, pid: u32, head_output: u64, length_output: u64) -> LinuxResult {
        let target = if pid == 0 || pid == self.thread.number() {
            self.thread
        } else {
            let snapshot = self.tasks.snapshot();
            let Some(thread) = snapshot.threads.iter().find(|thread| thread.id.number() == pid) else {
                return LinuxResult::Error(Errno::ESRCH);
            };
            if thread.process != self.process {
                return LinuxResult::Error(Errno::EPERM);
            }
            thread.id
        };
        let Ok(registration) = self.tasks.robust_list(target) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let head = registration.map_or(0, |value| value.head);
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (head_copyout, length_copyout) = match abi.stage_robust_list(head_output, length_output, head) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if let Err(error) = head_copyout.commit(&marshaller) {
            return LinuxResult::Error(error.errno());
        }
        if let Err(error) = length_copyout.commit(&marshaller) {
            return LinuxResult::Error(error.errno());
        }
        LinuxResult::Value(0)
    }

    pub(crate) fn futex(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let plan = match abi.futex(
            arguments[0],
            arguments[1] as u32,
            arguments[2] as u32,
            arguments[3],
            arguments[4],
            arguments[5] as u32,
        ) {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(futex) = &self.futex else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        futex.execute(self.process, self.thread, plan)
    }

    pub(crate) fn futex_waitv(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (vectors, deadline) = match abi.wait_vectors(
            arguments[0],
            arguments[1] as usize,
            arguments[2] as u32,
            arguments[3],
            arguments[4] as i32,
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(futex) = &self.futex else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        futex.wait_multiple(self.thread, &vectors, deadline)
    }

    pub(crate) fn nanosleep(&self, request: u64, remainder: u64) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (requested, remainder) = match abi.nanosleep(request, remainder) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.sleep_with(&abi, ClockIdentity::Monotonic, false, requested, remainder)
    }

    pub(crate) fn clock_nanosleep(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (clock, absolute, requested, remainder) =
            match abi.clock_nanosleep(arguments[0] as i32, arguments[1] as u32, arguments[2], arguments[3]) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
        self.sleep_with(&abi, clock, absolute, requested, remainder)
    }

    fn sleep_with(
        &self,
        abi: &TimeFutexAbi<'_, M>,
        clock: ClockIdentity,
        absolute: bool,
        requested: Timespec,
        remainder: u64,
    ) -> LinuxResult {
        let Some(sleep) = &self.sleep else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let interruption = self.blocking_interruption();
        let wake: Arc<dyn hl_task::SignalActivityWake> = Arc::new(SleepWake {
            tasks: self.tasks.clone(),
            thread: self.thread,
            interruption: interruption.clone(),
        });
        let _subscription = self.tasks.subscribe_signal_activity(wake.clone());
        wake.signal_activity_changed();
        match sleep.sleep(clock, absolute, requested, &interruption) {
            Ok(RuntimeSleepOutcome::Completed) => LinuxResult::Value(0),
            Ok(RuntimeSleepOutcome::Interrupted { remaining }) => {
                if let Err(error) = self.commit_remaining(abi, absolute, remainder, remaining) {
                    return LinuxResult::Error(error);
                }
                LinuxResult::Error(Errno::EINTR)
            }
            Err(()) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn commit_remaining(
        &self,
        abi: &TimeFutexAbi<'_, M>,
        absolute: bool,
        destination: u64,
        remaining: Timespec,
    ) -> Result<(), Errno> {
        if destination == 0 || absolute {
            return Ok(());
        }
        let staged = abi
            .stage_timespec(destination, remaining)
            .map_err(hl_linux::FutexMarshalError::errno)?;
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        staged.commit(&marshaller).map_err(hl_linux::FutexMarshalError::errno)
    }

    pub(crate) fn gettimeofday(&self, time: u64, timezone: u64) -> LinuxResult {
        let Some(provider) = &self.clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Ok(value) = provider.realtime_now() else {
            return LinuxResult::Error(Errno::EIO);
        };
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        if time != 0 {
            let staged = match abi.stage_timeval(
                time,
                value.seconds() as i64,
                i64::from(value.subsecond_nanoseconds() / 1_000),
            ) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if let Err(error) = staged.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        if timezone != 0 {
            let progress = marshaller.copy_to(timezone, &[0; 8]);
            if progress.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        LinuxResult::Value(0)
    }

    pub(crate) fn time(&self, output: u64) -> LinuxResult {
        let Some(provider) = &self.clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let seconds = match provider.realtime_now() {
            Ok(value) => value.seconds(),
            Err(_) => return LinuxResult::Error(Errno::EIO),
        };
        if output != 0 {
            let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
            if marshaller.copy_to(output, &seconds.to_le_bytes()).fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        LinuxResult::Value(seconds)
    }

    pub(crate) fn clock_gettime(&self, clock: i32, output: u64) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (identity, destination) = match abi.clock_read(clock, output) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(provider) = &self.clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let value = match identity {
            ClockIdentity::Realtime
            | ClockIdentity::RealtimeCoarse
            | ClockIdentity::RealtimeAlarm
            | ClockIdentity::Tai => provider.realtime_now().map_err(|_| ()),
            ClockIdentity::Monotonic
            | ClockIdentity::MonotonicRaw
            | ClockIdentity::MonotonicCoarse
            | ClockIdentity::BootTime
            | ClockIdentity::BootTimeAlarm
            | ClockIdentity::Cycle => provider
                .monotonic_now()
                .map(|value| Timespec::from_nanoseconds(value.nanoseconds()))
                .map_err(|_| ()),
            ClockIdentity::ProcessCpu => match &self.cpu_clock {
                Some(clock) => clock.aggregate().map_err(|_| ()),
                None => return LinuxResult::Error(Errno::ENOSYS),
            },
            ClockIdentity::ThreadCpu => match &self.cpu_clock {
                Some(clock) => clock.current().map_err(|_| ()),
                None => return LinuxResult::Error(Errno::ENOSYS),
            },
        };
        let Ok(value) = value else {
            return LinuxResult::Error(Errno::EIO);
        };
        let staged = match abi.stage_timespec(destination, value) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        match staged.commit(&marshaller) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn times(&self, output: u64) -> LinuxResult {
        const NANOS_PER_TICK: u64 = 10_000_000;
        let Some(cpu) = &self.cpu_clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Ok(process) = cpu.resource_usage(self.process, ResourceUsageScope::Process) else {
            return LinuxResult::Error(Errno::EIO);
        };
        let Ok(children) = cpu.resource_usage(self.process, ResourceUsageScope::Children) else {
            return LinuxResult::Error(Errno::EIO);
        };
        let child_user = Self::usage_ticks(children.user_seconds, children.user_microseconds);
        let child_system = Self::usage_ticks(children.system_seconds, children.system_microseconds);
        let ticks = [
            Self::usage_ticks(process.user_seconds, process.user_microseconds),
            Self::usage_ticks(process.system_seconds, process.system_microseconds),
            child_user,
            child_system,
        ];
        if output != 0 {
            let staged = match TimeFutexAbi::new(&self.memory, self.architecture).stage_process_times(output, ticks) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
                return LinuxResult::Error(error.errno());
            }
        }
        let Some(clock) = &self.clock else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        match clock.monotonic_now() {
            Ok(value) => LinuxResult::Value(value.nanoseconds() / NANOS_PER_TICK),
            Err(_) => LinuxResult::Error(Errno::EIO),
        }
    }

    fn usage_ticks(seconds: i64, microseconds: i64) -> i64 {
        seconds
            .saturating_mul(100)
            .saturating_add(microseconds.saturating_div(10_000))
    }

    pub(crate) const fn clock_settime(&self, clock: i32) -> LinuxResult {
        match clock {
            0 | 11 => LinuxResult::Error(Errno::EPERM),
            _ => LinuxResult::Error(Errno::EINVAL),
        }
    }

    pub(crate) fn clock_getres(&self, clock: i32, output: u64) -> LinuxResult {
        let abi = TimeFutexAbi::new(&self.memory, self.architecture);
        let (identity, destination) = match abi.clock_read(clock, output) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if destination == 0 {
            return LinuxResult::Value(0);
        }
        let nanoseconds = if matches!(identity, ClockIdentity::RealtimeCoarse | ClockIdentity::MonotonicCoarse) {
            1_000_000
        } else {
            1
        };
        let staged = match abi.stage_timespec(destination, Timespec::new(0, nanoseconds).unwrap()) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let marshaller = hl_linux::GuestMarshaller::new(&self.memory, self.architecture);
        match staged.commit(&marshaller) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn clock_adjtime(&self, clock: i32, address: u64) -> LinuxResult {
        if !(0..=11).contains(&clock) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.adjtimex(address)
    }

    pub(crate) fn adjtimex(&self, address: u64) -> LinuxResult {
        // Linux's LP64 kernel timex layout through `tick` occupies 96 bytes.
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let mut bytes = [0_u8; 96];
        let progress = marshaller.copy_from(address, &mut bytes);
        if progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if u32::from_le_bytes(bytes[..4].try_into().unwrap()) != 0 {
            return LinuxResult::Error(Errno::EPERM);
        }

        bytes[8..40].fill(0);
        bytes[24..32].copy_from_slice(&16_384_i64.to_le_bytes());
        bytes[32..40].copy_from_slice(&16_384_i64.to_le_bytes());
        bytes[40..44].copy_from_slice(&0x40_i32.to_le_bytes());
        bytes[48..56].copy_from_slice(&2_i64.to_le_bytes());
        bytes[56..64].copy_from_slice(&1_i64.to_le_bytes());
        bytes[64..72].copy_from_slice(&32_768_000_i64.to_le_bytes());
        if let Some(provider) = &self.clock
            && let Ok(now) = provider.realtime_now()
        {
            bytes[72..80].copy_from_slice(&(now.seconds() as i64).to_le_bytes());
            bytes[80..88].copy_from_slice(&i64::from(now.subsecond_nanoseconds() / 1_000).to_le_bytes());
        }
        bytes[88..96].copy_from_slice(&10_000_i64.to_le_bytes());

        let progress = marshaller.copy_to(address, &bytes);
        if progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }
}
