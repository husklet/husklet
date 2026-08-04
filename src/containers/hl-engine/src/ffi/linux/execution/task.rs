//! Task, signal, time, and interruption composition.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::{Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hl_linux::{Errno, LinuxResult, SyscallOperation, TaskSignalTimeSyscalls};
use hl_runtime::RuntimeProcessSyscalls;

use super::process_memory::ProcessMemory;

/// Marks `sched_yield` as a completed cooperative scheduling boundary.
///
/// The execution scheduler already runs one bounded slice or syscall per turn
/// and rotates `ThreadSet::next` after this adapter returns. No host sleep or
/// additional task state is needed here.
pub(super) struct CooperativeYield;

#[derive(Debug)]
pub(super) struct HostCounter;

impl hl_execution::ArchitecturalCounter for HostCounter {
    fn read(&self) -> u64 {
        crate::native::HostSyscalls::clock_ns(
            &crate::ffi::LinuxHost,
            crate::native::ClockKind::Monotonic,
        )
        .unwrap_or(0)
    }
}

impl hl_runtime::RuntimeYieldPort for CooperativeYield {
    fn yield_task(&self, _: hl_task::ProcessId, _: hl_task::ThreadId) -> Result<(), ()> {
        Ok(())
    }
}

pub(super) struct ClockIdentity {
    deadlines: Arc<super::readiness::deadline::Queue>,
    resource: hl_runtime::EventResourceKey,
    tasks: Arc<hl_task::TaskRegistry>,
}

impl std::fmt::Debug for ClockIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ClockIdentity").field("resource", &self.resource).finish()
    }
}

impl hl_runtime::TimerClockSource for ClockIdentity {
    fn realtime_generation(&self) -> u64 {
        0
    }
    fn schedule_callback(
        &self,
        deadline_nanoseconds: u64,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<u64, hl_time::ClockError> {
        let deadline = self.project_deadline(deadline_nanoseconds)?;
        self.deadlines.schedule_callback(deadline, callback)
    }
    fn cancel_wake(&self, token: u64) {
        self.deadlines.cancel(token);
    }
}

impl ClockIdentity {
    fn project_deadline(&self, deadline_nanoseconds: u64) -> Result<u64, hl_time::ClockError> {
        let queue_now = u64::try_from(self.deadlines.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let host_now =
            crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::Monotonic)
                .map_err(|_| hl_time::ClockError::Failed)?;
        Self::queue_deadline(queue_now, host_now, deadline_nanoseconds)
    }
}

#[derive(Debug)]
pub(super) struct ClockSource(pub(super) Arc<ClockIdentity>);

pub(super) struct SleepPort(pub(super) Arc<super::readiness::deadline::Queue>);

impl hl_runtime::RuntimeSleepPort for SleepPort {
    fn sleep(
        &self,
        clock: hl_linux::ClockIdentity,
        absolute: bool,
        requested: hl_time::Timespec,
        interruption: &hl_sync::Interruption,
    ) -> Result<hl_runtime::RuntimeSleepOutcome, ()> {
        let value = requested.checked_nanoseconds().ok_or(())?;
        let now = u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let deadline = if !absolute {
            now.checked_add(value).ok_or(())?
        } else {
            let current = match clock {
                hl_linux::ClockIdentity::Realtime => {
                    let realtime = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| ())?;
                    u64::try_from(realtime.as_nanos()).map_err(|_| ())?
                }
                hl_linux::ClockIdentity::ProcessCpu => {
                    crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::ProcessCpu)
                        .map_err(|_| ())?
                }
                _ => crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::Monotonic)
                    .map_err(|_| ())?,
            };
            now.checked_add(value.saturating_sub(current)).ok_or(())?
        };
        if self.0.wait_interruptible(deadline, interruption)? {
            let now = u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let remaining = deadline.saturating_sub(now);
            Ok(hl_runtime::RuntimeSleepOutcome::Interrupted {
                remaining: hl_time::Timespec::from_nanoseconds(remaining),
            })
        } else {
            Ok(hl_runtime::RuntimeSleepOutcome::Completed)
        }
    }
}

impl hl_runtime::TimerEventSource for ClockSource {
    fn clock(
        &self,
    ) -> Result<(hl_runtime::EventResourceKey, Arc<dyn hl_runtime::TimerClockSource>), hl_runtime::EventSourceError>
    {
        Ok((self.0.resource, self.0.clone()))
    }
}

impl ClockIdentity {
    fn queue_deadline(queue_now: u64, host_now: u64, host_deadline: u64) -> Result<u64, hl_time::ClockError> {
        queue_now
            .checked_add(host_deadline.saturating_sub(host_now))
            .ok_or(hl_time::ClockError::Failed)
    }

    pub(super) fn new(
        _uid: u64,
        _gid: u64,
        process: hl_task::ProcessId,
        deadlines: Arc<super::readiness::deadline::Queue>,
        tasks: Arc<hl_task::TaskRegistry>,
    ) -> Self {
        let resource = hl_runtime::EventResourceKey::new(0x1000_0000_0000_0000 | u64::from(process.number()))
            .expect("process identities are nonzero");
        Self { deadlines, resource, tasks }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use super::{ClockIdentity, CooperativeYield, TaskAdapter};
    use hl_event::TimerClockSource;
    use hl_runtime::RuntimeYieldPort;
    use hl_time::MonotonicClock;

    #[test]
    fn timer_wake_projects_host_deadline_to_queue_origin() {
        assert_eq!(ClockIdentity::queue_deadline(25, 10_000, 10_075).unwrap(), 100);
        assert_eq!(ClockIdentity::queue_deadline(25, 10_000, 9_999).unwrap(), 25);
        assert!(ClockIdentity::queue_deadline(u64::MAX, 10_000, 10_001).is_err());
    }

    #[test]
    fn cooperative_yield_completes_without_host_wait() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let thread = hl_task::ThreadId::from_wire(1, 1).unwrap();
        assert_eq!(CooperativeYield.yield_task(process, thread), Ok(()));
    }

    #[test]
    fn legacy_pause_routes() {
        assert!(TaskAdapter::signal_wait("pause"));
        assert!(TaskAdapter::signal_wait("rt_sigsuspend"));
        assert!(!TaskAdapter::signal_wait("mkdirat"));
    }

    #[test]
    fn projected_timer_wake_notifies_deadline_descriptor() {
        let deadlines = super::super::readiness::deadline::Queue::new().unwrap();
        let clock = ClockIdentity::new(
            0,
            0,
            hl_task::ProcessId::from_wire(1, 1).unwrap(),
            deadlines.clone(),
            Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap()),
        );
        let now = clock.monotonic_now().unwrap().nanoseconds();
        let notified = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_notified = Arc::clone(&notified);
        clock
            .schedule_callback(
                now + 10_000_000,
                Arc::new(move || callback_notified.store(true, std::sync::atomic::Ordering::Release)),
            )
            .unwrap();
        let mut descriptor = libc::pollfd {
            fd: deadlines.descriptor(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the call duration.
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1_000) }, 1);
        assert_ne!(descriptor.revents & libc::POLLIN, 0);
        assert!(notified.load(std::sync::atomic::Ordering::Acquire));
    }
}

impl hl_time::MonotonicClock for ClockIdentity {
    fn monotonic_now(&self) -> Result<hl_time::MonotonicInstant, hl_time::ClockError> {
        Ok(hl_time::MonotonicInstant::from_nanoseconds(
            crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::Monotonic)
                .map_err(|_| hl_time::ClockError::Failed)?,
        ))
    }

    fn sleep_until(&self, deadline: hl_time::Deadline) -> Result<(), hl_time::ClockError> {
        let now = self.monotonic_now()?;
        let remaining = deadline.remaining_at(now).nanoseconds();
        if remaining != 0 {
            std::thread::sleep(Duration::from_nanos(remaining));
        }
        Ok(())
    }
}

impl hl_time::RealtimeClock for ClockIdentity {
    fn realtime_now(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| hl_time::ClockError::Failed)?;
        hl_time::Timespec::new(duration.as_secs(), duration.subsec_nanos()).ok_or(hl_time::ClockError::Failed)
    }
}

impl hl_runtime::CpuClockPort for ClockIdentity {
    fn aggregate(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        self.cpu(crate::native::ClockKind::ProcessCpu)
    }

    fn current(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        self.cpu(crate::native::ClockKind::ThreadCpu)
    }

    fn user(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the supplied rusage on success and retains no pointer.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return Err(hl_time::ClockError::Failed);
        }
        // SAFETY: the successful call initialized the complete structure.
        let usage = unsafe { usage.assume_init() };
        let seconds = u64::try_from(usage.ru_utime.tv_sec).map_err(|_| hl_time::ClockError::Failed)?;
        let microseconds = u32::try_from(usage.ru_utime.tv_usec).map_err(|_| hl_time::ClockError::Failed)?;
        hl_time::Timespec::new(seconds, microseconds.saturating_mul(1_000)).ok_or(hl_time::ClockError::Failed)
    }

    fn resource_usage(
        &self,
        process: hl_task::ProcessId,
        scope: hl_runtime::ResourceUsageScope,
    ) -> Result<hl_linux::ResourceUsage, hl_time::ClockError> {
        let who = match scope {
            hl_runtime::ResourceUsageScope::Process => libc::RUSAGE_SELF,
            hl_runtime::ResourceUsageScope::Children => libc::RUSAGE_CHILDREN,
        };
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the supplied rusage on success and retains no pointer.
        if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
            return Err(hl_time::ClockError::Failed);
        }
        // SAFETY: the successful call initialized the complete structure.
        let usage = unsafe { usage.assume_init() };
        #[cfg(target_os = "macos")]
        let maximum_resident_set = usage.ru_maxrss / 1024;
        #[cfg(not(target_os = "macos"))]
        let maximum_resident_set = usage.ru_maxrss;
        let guest = self.tasks.cpu_usage(process).map_err(|_| hl_time::ClockError::Failed)?;
        let nanoseconds = match scope {
            hl_runtime::ResourceUsageScope::Process => guest.self_nanoseconds,
            hl_runtime::ResourceUsageScope::Children => guest.children_nanoseconds,
        };
        // The portable thread CPU clock exposes total execution CPU but not a
        // user/system split. Account that measured total as user time and do
        // not fabricate system time.
        Ok(hl_linux::ResourceUsage {
            user_seconds: i64::try_from(nanoseconds / 1_000_000_000).unwrap_or(i64::MAX),
            user_microseconds: i64::from(((nanoseconds % 1_000_000_000) / 1_000) as u32),
            system_seconds: 0,
            system_microseconds: 0,
            maximum_resident_set,
            minor_faults: usage.ru_minflt,
            major_faults: usage.ru_majflt,
            voluntary_switches: usage.ru_nvcsw,
            involuntary_switches: usage.ru_nivcsw,
        })
    }
}

impl ClockIdentity {
    fn cpu(&self, kind: crate::native::ClockKind) -> Result<hl_time::Timespec, hl_time::ClockError> {
        let nanoseconds = crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, kind)
            .map_err(|_| hl_time::ClockError::Failed)?;
        Ok(hl_time::Timespec::from_nanoseconds(nanoseconds))
    }
}

pub(super) struct FutexInterrupt {
    threads: Mutex<BTreeMap<hl_task::ThreadId, Weak<hl_sync::Interruption>>>,
}

impl FutexInterrupt {
    pub(super) fn new() -> Self {
        Self {
            threads: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn register(&self, thread: hl_task::ThreadId, interruption: Arc<hl_sync::Interruption>) {
        self.threads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(thread, Arc::downgrade(&interruption));
    }
}

impl hl_runtime::FutexInterruptionSource for FutexInterrupt {
    fn interruption(&self, thread: hl_task::ThreadId) -> Option<Arc<hl_sync::Interruption>> {
        self.threads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&thread)
            .and_then(Weak::upgrade)
    }

    fn identity(&self, number: u32) -> Option<hl_task::ThreadId> {
        self.threads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .find(|thread| thread.number() == number)
            .copied()
    }
}

pub(super) struct TaskAdapter {
    process: RuntimeProcessSyscalls<ProcessMemory>,
}

impl TaskAdapter {
    pub(super) const fn new(process: RuntimeProcessSyscalls<ProcessMemory>) -> Self {
        Self { process }
    }

    fn signal_wait(name: &str) -> bool {
        matches!(name, "pause" | "rt_sigtimedwait" | "rt_sigsuspend")
    }
}

impl TaskSignalTimeSyscalls for TaskAdapter {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "exit" | "exit_group" | "getpid" | "gettid" | "getppid" | "futex" | "getuid" | "geteuid" | "getgid"
            | "getegid" | "getresuid" | "getresgid" | "getgroups" | "setgroups" | "setuid" | "setgid" | "setreuid"
            | "setregid" | "setresuid" | "setresgid" | "setfsuid" | "setfsgid" | "getpgid" | "getpgrp" | "getsid" | "setpgid"
            | "setsid" | "set_tid_address" | "set_robust_list" | "get_robust_list" | "wait4" | "waitid" | "getrlimit"
            | "setrlimit" | "prlimit64" | "prctl" | "personality" | "fanotify_init" | "fanotify_mark" | "execve"
            | "execveat"
            | "bpf" | "userfaultfd" | "io_uring_setup" | "io_uring_enter" | "io_uring_register" | "ptrace"
            | "umask" => {
                self.process.handle(operation, arguments)
            }
            "unshare" | "setns" => self.process.handle(operation, arguments),
            "capget" | "capset" | "pidfd_open" | "pidfd_getfd" | "pidfd_send_signal" => {
                self.process.handle(operation, arguments)
            }
            "kill" | "tkill" | "tgkill" | "rt_sigaction" | "rt_sigprocmask" | "rt_sigpending" | "rt_sigqueueinfo"
            | "rt_tgsigqueueinfo"
            | "rt_sigreturn" | "sigaltstack" => {
                self.process.handle(operation, arguments)
            }
            name if Self::signal_wait(name) => self.process.handle(operation, arguments),
            "alarm" | "nanosleep" | "clock_nanosleep" | "getitimer" | "setitimer" | "timer_create" | "timer_settime"
            | "timer_gettime" | "timer_getoverrun" | "timer_delete" | "uname" | "sysinfo" | "sethostname"
            | "setdomainname" => self.process.handle(operation, arguments),
            "clock_gettime" | "clock_getres" | "clock_settime" | "clock_adjtime" | "adjtimex" | "gettimeofday" | "time"
            | "getrusage" | "times" => {
                self.process.handle(operation, arguments)
            }
            "sched_setparam"
            | "sched_setscheduler"
            | "sched_getscheduler"
            | "sched_getparam"
            | "sched_get_priority_max"
            | "sched_get_priority_min"
            | "sched_rr_get_interval" => self.process.handle(operation, arguments),
            "sched_setattr" | "sched_getattr" => self.process.handle(operation, arguments),
            "sched_setaffinity" | "sched_getaffinity" | "sched_yield" | "getcpu" => {
                self.process.handle(operation, arguments)
            }
            "setpriority" | "getpriority" => self.process.handle(operation, arguments),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}
