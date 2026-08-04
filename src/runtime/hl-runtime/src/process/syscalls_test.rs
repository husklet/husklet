use super::RuntimeProcessSyscalls;
use crate::TaskSignalQueue;
use hl_event::{SignalMask as EventSignalMask, SignalQueue};
use hl_linux::{
    GuestAccess, GuestArchitecture, GuestFault, GuestMemory, LinuxResult, SyscallFamily, SyscallOperation,
    TaskSignalTimeSyscalls,
};
use hl_task::{
    Limit, PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, Resource, SignalAction, SignalDisposition,
    SignalInfo, SignalMask, SignalNumber, TaskRegistry,
};
use std::sync::{Arc, Mutex};
#[path = "../signal/wait_test.rs"]
mod signal_wait_tests;
struct FixedClock;
struct FixedCpuClock;
struct InterruptedSleep;
#[derive(Default)]
struct AlarmClock(Mutex<u64>);
#[derive(Default)]
struct YieldPort(Mutex<Vec<(hl_task::ProcessId, hl_task::ThreadId)>>);

impl crate::RuntimeYieldPort for YieldPort {
    fn yield_task(&self, process: hl_task::ProcessId, thread: hl_task::ThreadId) -> Result<(), ()> {
        self.0.lock().unwrap().push((process, thread));
        Ok(())
    }
}
#[derive(Default)]
struct WakePort(Mutex<Vec<hl_linux::FutexPlan>>);

impl crate::RuntimeFutexPort for WakePort {
    fn execute(&self, _: hl_task::ProcessId, _: hl_task::ThreadId, plan: hl_linux::FutexPlan) -> LinuxResult {
        self.0.lock().unwrap().push(plan);
        LinuxResult::Value(1)
    }
}
impl hl_time::MonotonicClock for FixedClock {
    fn monotonic_now(&self) -> Result<hl_time::MonotonicInstant, hl_time::ClockError> {
        Ok(hl_time::MonotonicInstant::from_nanoseconds(2_000_000_003))
    }
}

impl hl_time::RealtimeClock for FixedClock {
    fn realtime_now(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        Ok(hl_time::Timespec::new(7, 11).unwrap())
    }
}

impl crate::CpuClockPort for FixedCpuClock {
    fn aggregate(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        Ok(hl_time::Timespec::new(13, 17).unwrap())
    }

    fn current(&self) -> Result<hl_time::Timespec, hl_time::ClockError> {
        Ok(hl_time::Timespec::new(19, 23).unwrap())
    }
}

impl crate::RuntimeSleepPort for InterruptedSleep {
    fn sleep(
        &self,
        _: hl_linux::ClockIdentity,
        _: bool,
        _: hl_time::Timespec,
        _: &hl_sync::Interruption,
    ) -> Result<crate::RuntimeSleepOutcome, ()> {
        Ok(crate::RuntimeSleepOutcome::Interrupted {
            remaining: hl_time::Timespec::new(1, 5).unwrap(),
        })
    }
}

impl crate::AlarmScheduler for AlarmClock {
    fn now(&self) -> u64 {
        *self.0.lock().unwrap()
    }

    fn schedule(&self, _: u64, _: Arc<dyn Fn() + Send + Sync>) -> Result<u64, ()> {
        Ok(1)
    }

    fn cancel(&self, _: u64) {}
}

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl Memory {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(vec![0; 256])))
    }

    fn put(&self, address: usize, bytes: &[u8]) {
        self.0.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: usize, length: usize) -> Vec<u8> {
        self.0.lock().unwrap()[address..address + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        if (address as usize).saturating_add(length) > self.0.lock().unwrap().len() {
            Err(GuestFault { address, access })
        } else {
            Ok(length)
        }
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        let bytes = self.0.lock().unwrap();
        let Some(source) = bytes.get(start..start.saturating_add(output.len())) else {
            return Err(GuestFault {
                address,
                access: GuestAccess::Read,
            });
        };
        output.copy_from_slice(source);
        Ok(output.len())
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if (address as usize).saturating_add(input.len()) > self.0.lock().unwrap().len() {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.put(address as usize, input);
        Ok(input.len())
    }
}
#[path = "robust_test.rs"]
mod robust_tests;

struct Fixture {
    tasks: Arc<TaskRegistry>,
    process: hl_task::ProcessId,
    thread: hl_task::ThreadId,
    memory: Memory,
}

impl Fixture {
    fn new() -> Self {
        let tasks = Arc::new(
            TaskRegistry::new(RegistryConfig {
                max_groups: 65_536,
                online_cpus: 10,
                ..RegistryConfig::default()
            })
            .unwrap(),
        );
        let credentials = ProcessCredentials::new(0, 0, &[4, 9], 65_536).unwrap();
        let mut limits = ProcessLimits::default();
        limits.set(Resource::OpenFiles, Limit::new(64, 128).unwrap());
        let (process, thread) = tasks.create_init(credentials, limits).unwrap();
        Self {
            tasks,
            process,
            thread,
            memory: Memory::new(),
        }
    }

    fn runtime(&self, architecture: GuestArchitecture, thread: hl_task::ThreadId) -> RuntimeProcessSyscalls<Memory> {
        self.runtime_for(architecture, self.process, thread)
    }

    fn runtime_for(
        &self,
        architecture: GuestArchitecture,
        process: hl_task::ProcessId,
        thread: hl_task::ThreadId,
    ) -> RuntimeProcessSyscalls<Memory> {
        RuntimeProcessSyscalls::new(self.tasks.clone(), process, thread, self.memory.clone(), architecture)
    }

    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::TaskSignalTime,
        }
    }
}

#[test]
fn identity_work_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("getpid"), [0; 6]),
            LinuxResult::Value(fixture.process.number() as u64),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("gettid"), [0; 6]),
            LinuxResult::Value(fixture.thread.number() as u64),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getgroups"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(2),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getgroups"), [2, 32, 0, 0, 0, 0]),
            LinuxResult::Value(2),
        );
        assert_eq!(&fixture.memory.0.lock().unwrap()[32..40], &[4, 0, 0, 0, 9, 0, 0, 0]);
        fixture.memory.put(64, &[7, 0, 0, 0, 8, 0, 0, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("setgroups"), [2, 64, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setresuid"), [10, 11, 12, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getresuid"), [96, 100, 104, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            &fixture.memory.0.lock().unwrap()[96..108],
            &[10, 0, 0, 0, 11, 0, 0, 0, 12, 0, 0, 0,]
        );
        assert_eq!(
            runtime.handle(Fixture::operation("umask"), [0o077, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0o022),
        );
    }
}

#[test]
fn uname_identity_isas() {
    for (architecture, machine) in [
        (GuestArchitecture::Aarch64, b"aarch64".as_slice()),
        (GuestArchitecture::X86_64, b"x86_64".as_slice()),
    ] {
        let fixture = Fixture::new();
        fixture.memory.0.lock().unwrap().resize(1024, 0);
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("uname"), [128, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let bytes = fixture.memory.0.lock().unwrap();
        assert_eq!(&bytes[128..133], b"Linux");
        assert_eq!(&bytes[193..196], b"jit");
        assert_eq!(&bytes[388..388 + machine.len()], machine);
        assert_eq!(bytes[128 + 5], 0);
        assert_eq!(bytes[128 + 5 * 65], 0);
        drop(bytes);
        assert_eq!(
            runtime.handle(Fixture::operation("uname"), [900, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
    }
}

#[test]
fn uts_mutation() {
    let fixture = Fixture::new();
    let mut credentials = fixture.tasks.credentials(fixture.process).unwrap();
    credentials.capabilities.effective |= hl_task::CapabilitySets::SYS_ADMIN;
    fixture.tasks.replace_credentials(fixture.process, credentials).unwrap();
    fixture.memory.0.lock().unwrap().resize(512, 0);
    fixture.memory.put(8, b"alpha");
    fixture.memory.put(24, b"example");
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("sethostname"), [8, 5, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("setdomainname"), [24, 7, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("uname"), [64, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    let bytes = fixture.memory.0.lock().unwrap();
    assert_eq!(&bytes[64 + 65..64 + 70], b"alpha");
    assert_eq!(&bytes[64 + 5 * 65..64 + 5 * 65 + 7], b"example");
    drop(bytes);
    assert_eq!(
        runtime.handle(Fixture::operation("sethostname"), [8, 65, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL)
    );
}

#[test]
fn credential_drop_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("setfsuid"), [7, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setfsuid"), [u64::MAX, 0, 0, 0, 0, 0]),
            LinuxResult::Value(7)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setresgid"), [30, 31, 32, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setresuid"), [20, 21, 22, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setuid"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EPERM)
        );
        let credentials = fixture
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id == fixture.process)
            .unwrap()
            .credentials;
        assert_eq!(
            (
                credentials.real_user,
                credentials.effective_user,
                credentials.saved_user,
                credentials.filesystem_user
            ),
            (20, 21, 22, 21)
        );
        assert_eq!(
            (
                credentials.real_group,
                credentials.effective_group,
                credentials.saved_group,
                credentials.filesystem_group
            ),
            (30, 31, 32, 31)
        );
        assert_eq!(credentials.capabilities.effective, hl_task::CapabilitySets::CONTAINER);
        assert_eq!(credentials.capabilities.permitted, hl_task::CapabilitySets::CONTAINER);
        assert_eq!(credentials.setid_authority(), hl_task::SetIdAuthority::None);
    }
}

#[test]
fn sysinfo_metrics_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock));
        assert_eq!(
            runtime.handle(Fixture::operation("sysinfo"), [64, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let bytes = fixture.memory.0.lock().unwrap();
        assert_eq!(u64::from_le_bytes(bytes[64..72].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(bytes[144..146].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(bytes[96..104].try_into().unwrap()), 8_u64 << 30);
        assert_eq!(&bytes[72..96], &[0; 24]);
        assert_eq!(u64::from_le_bytes(bytes[104..112].try_into().unwrap()), 2_u64 << 30);
        assert_eq!(&bytes[112..144], &[0; 32]);
        assert_eq!(u32::from_le_bytes(bytes[168..172].try_into().unwrap()), 1);
        drop(bytes);
        assert_eq!(
            runtime.handle(Fixture::operation("sysinfo"), [200, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
    }

    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("sysinfo"), [64, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
}

#[test]
fn scheduler_identity_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let scheduler = Arc::new(YieldPort::default());
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_yield_port(scheduler.clone());
        assert_eq!(
            runtime.handle(Fixture::operation("sched_yield"), [0; 6]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            scheduler.0.lock().unwrap().as_slice(),
            &[(fixture.process, fixture.thread)]
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getcpu"), [32, 36, u64::MAX, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        let bytes = fixture.memory.0.lock().unwrap();
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 0);
        drop(bytes);
        assert_eq!(
            runtime.handle(Fixture::operation("getcpu"), [40, 254, u64::MAX, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
        assert_eq!(
            u32::from_le_bytes(fixture.memory.0.lock().unwrap()[40..44].try_into().unwrap()),
            0,
        );
    }

    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("sched_yield"), [0; 6]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
}

#[test]
fn affinity_threads() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let clone = fixture.tasks.begin_clone_thread(fixture.thread).unwrap();
        let worker = fixture.tasks.commit_clone_thread(clone).unwrap();
        let mut runtime = fixture.runtime(architecture, fixture.thread);

        fixture.memory.put(32, &[8, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_setaffinity"),
                [worker.number() as u64, 8, 32, 0, 0, 0]
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_getaffinity"),
                [worker.number() as u64, 8, 64, 0, 0, 0]
            ),
            LinuxResult::Value(8),
        );
        assert_eq!(&fixture.memory.get(64, 8), &[8, 0, 0, 0, 0, 0, 0, 0]);

        assert_eq!(
            runtime.handle(Fixture::operation("sched_getaffinity"), [u64::MAX, 7, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getaffinity"), [99_999, 8, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getaffinity"), [0, 8, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setaffinity"), [99_999, 8, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setaffinity"), [0, 0, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );

        let mut worker_runtime = fixture.runtime(architecture, worker);
        assert_eq!(
            worker_runtime.handle(Fixture::operation("getcpu"), [96, 100, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(u32::from_le_bytes(fixture.memory.get(96, 4).try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(fixture.memory.get(100, 4).try_into().unwrap()), 0);
    }
}

#[test]
fn realtime_schedule_rejected() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        fixture.memory.put(32, &1_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setscheduler"), [0, 1, 32, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setscheduler"), [0, 1, 255, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
    }
}

#[test]
fn schedule_round_trips() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let worker = fixture
            .tasks
            .commit_clone_thread(fixture.tasks.begin_clone_thread(fixture.thread).unwrap())
            .unwrap();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        fixture.memory.put(32, &0_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_setscheduler"),
                [worker.number() as u64, 3, 32, 0, 0, 0]
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_getscheduler"),
                [worker.number() as u64, 0, 0, 0, 0, 0]
            ),
            LinuxResult::Value(3),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getscheduler"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_setparam"),
                [worker.number() as u64, 32, 0, 0, 0, 0]
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(
                Fixture::operation("sched_getparam"),
                [worker.number() as u64, 36, 0, 0, 0, 0]
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(fixture.memory.get(36, 4), 0_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getparam"), [99_999, 255, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setscheduler"), [99_999, 99, 32, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH),
        );
    }
}

#[test]
fn schedule_bounds_interval() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        for (policy, minimum, maximum) in [(0, 0, 0), (1, 1, 99), (2, 1, 99), (3, 0, 0), (5, 0, 0), (6, 0, 0)] {
            assert_eq!(
                runtime.handle(Fixture::operation("sched_get_priority_min"), [policy, 0, 0, 0, 0, 0]),
                LinuxResult::Value(minimum)
            );
            assert_eq!(
                runtime.handle(Fixture::operation("sched_get_priority_max"), [policy, 0, 0, 0, 0, 0]),
                LinuxResult::Value(maximum)
            );
        }
        assert_eq!(
            runtime.handle(Fixture::operation("sched_get_priority_min"), [99, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_rr_get_interval"), [0, 48, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(u64::from_le_bytes(fixture.memory.get(48, 8).try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(fixture.memory.get(56, 8).try_into().unwrap()),
            100_000_000
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_rr_get_interval"), [99_999, 255, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_rr_get_interval"), [0, 255, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_rr_get_interval"), [u64::MAX, 255, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
    }
}

#[test]
fn schedule_attr_round_trip() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let mut attr = [0_u8; 48];
        attr[0..4].copy_from_slice(&48_u32.to_le_bytes());
        attr[4..8].copy_from_slice(&3_u32.to_le_bytes());
        attr[16..20].copy_from_slice(&5_i32.to_le_bytes());
        fixture.memory.put(64, &attr);
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setattr"), [0, 64, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getscheduler"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(3)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getattr"), [0, 128, 48, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(u32::from_le_bytes(fixture.memory.get(128, 4).try_into().unwrap()), 48);
        assert_eq!(u32::from_le_bytes(fixture.memory.get(132, 4).try_into().unwrap()), 3);
        assert_eq!(i32::from_le_bytes(fixture.memory.get(144, 4).try_into().unwrap()), 5);
        assert_eq!(
            runtime.handle(Fixture::operation("getpriority"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(15)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("setpriority"), [0, 0, 100, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getpriority"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getattr"), [0, 128, 48, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(i32::from_le_bytes(fixture.memory.get(144, 4).try_into().unwrap()), 19);
        fixture.memory.put(32, &0_i32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setscheduler"), [0, 5, 32, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getpriority"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(1)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getattr"), [0, 128, 48, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(i32::from_le_bytes(fixture.memory.get(144, 4).try_into().unwrap()), 19);
        assert_eq!(
            runtime.handle(Fixture::operation("getpriority"), [99, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("getpriority"), [0, 99_999, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH)
        );
        attr[4..8].copy_from_slice(&1_u32.to_le_bytes());
        attr[20..24].copy_from_slice(&50_i32.to_le_bytes());
        fixture.memory.put(64, &attr);
        assert_eq!(
            runtime.handle(Fixture::operation("sched_setattr"), [0, 64, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EPERM)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getscheduler"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(5)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getattr"), [99_999, 255, 48, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::ESRCH)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("sched_getattr"), [0, 255, 48, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
    }
}

#[test]
fn gettid_process_leader() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_clone_thread(fixture.thread).unwrap();
    let worker = fixture.tasks.commit_clone_thread(plan).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, worker);
    assert_ne!(worker, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("gettid"), [0; 6]),
        LinuxResult::Value(worker.number() as u64),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("getpid"), [0; 6]),
        LinuxResult::Value(fixture.process.number() as u64),
    );
}

#[test]
fn namespace_failures_exact() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [0; 6]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [0x0000_0400, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [u64::MAX, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [0x0400_0000, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("setns"), [u64::MAX, u64::MAX, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EBADF),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("setns"), [3, 7, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("setns"), [3, 0x0400_0000, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
}

#[test]
fn userns_unadvertised() {
    let fixture = Fixture::new();
    let initial = fixture.tasks.namespaces(fixture.process).unwrap().user;
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [0x1000_0000, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(fixture.tasks.namespaces(fixture.process).unwrap().user, initial);
}

#[test]
fn uts_namespace_rebinds() {
    let fixture = Fixture::new();
    let mut credentials = fixture
        .tasks
        .snapshot()
        .processes
        .into_iter()
        .find(|process| process.id == fixture.process)
        .unwrap()
        .credentials;
    credentials.capabilities.effective |= hl_task::CapabilitySets::SYS_ADMIN;
    credentials.capabilities.permitted |= hl_task::CapabilitySets::SYS_ADMIN;
    fixture.tasks.replace_credentials(fixture.process, credentials).unwrap();
    let descriptors = Arc::new(hl_descriptor::DescriptorTable::new(8).unwrap());
    let handles = Arc::new(crate::NamespaceHandleRegistry::new());
    let initial = fixture.tasks.namespaces(fixture.process).unwrap().uts;
    let descriptor = handles.install(&descriptors, initial).unwrap();
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64, fixture.thread)
        .with_namespace_handles(descriptors, handles);
    assert_eq!(
        runtime.handle(Fixture::operation("unshare"), [0x0400_0000, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_ne!(fixture.tasks.namespaces(fixture.process).unwrap().uts, initial);
    assert_eq!(
        runtime.handle(
            Fixture::operation("setns"),
            [descriptor as u64, 0x0400_0000, 0, 0, 0, 0]
        ),
        LinuxResult::Value(0),
    );
    assert_eq!(fixture.tasks.namespaces(fixture.process).unwrap().uts, initial);
}

#[test]
fn clear_tid_exit() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (process, worker) = fixture.tasks.commit_fork_process(plan).unwrap();
    fixture.memory.put(32, &worker.number().to_le_bytes());
    let wake = Arc::new(WakePort::default());
    let mut runtime = RuntimeProcessSyscalls::new(
        fixture.tasks.clone(),
        process,
        worker,
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    )
    .with_futex_port(wake.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("set_tid_address"), [32, 0, 0, 0, 0, 0]),
        LinuxResult::Value(worker.number() as u64),
    );
    assert_eq!(fixture.tasks.clear_tid(worker).unwrap(), Some(32));
    assert_eq!(
        runtime.handle(Fixture::operation("exit"), [0; 6]),
        LinuxResult::Value(0),
    );
    assert_eq!(&fixture.memory.0.lock().unwrap()[32..36], &[0; 4]);
    let wakes = wake.0.lock().unwrap();
    assert_eq!(wakes.len(), 2);
    assert_eq!(wakes[0].address, 32);
    assert_eq!(wakes[0].value, 1);
    assert!(wakes[0].private);
    assert_eq!(wakes[1].address, 32);
    assert_eq!(wakes[1].value, 1);
    assert!(!wakes[1].private);
}

#[test]
fn clear_fault_consumed() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (process, worker) = fixture.tasks.commit_fork_process(plan).unwrap();
    let wake = Arc::new(WakePort::default());
    let mut runtime = RuntimeProcessSyscalls::new(
        fixture.tasks.clone(),
        process,
        worker,
        fixture.memory.clone(),
        GuestArchitecture::Aarch64,
    )
    .with_futex_port(wake.clone());
    assert_eq!(
        runtime.handle(Fixture::operation("set_tid_address"), [u64::MAX, 0, 0, 0, 0, 0]),
        LinuxResult::Value(worker.number() as u64),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("exit"), [0; 6]),
        LinuxResult::Value(0),
    );
    assert!(wake.0.lock().unwrap().is_empty());
}

#[test]
fn injected_timespecs_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock));
        assert_eq!(
            runtime.handle(Fixture::operation("clock_gettime"), [0, 32, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            &fixture.memory.0.lock().unwrap()[32..48],
            &[7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_gettime"), [1, 64, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_getres"), [1, 96, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
    }
}

#[test]
fn time_writes_epoch_seconds_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock));
        assert_eq!(
            runtime.handle(Fixture::operation("time"), [32, 0, 0, 0, 0, 0]),
            LinuxResult::Value(7),
        );
        assert_eq!(fixture.memory.get(32, 8), 7_u64.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("time"), [u64::MAX, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
    }
}

#[test]
fn clock_ids_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        check_clock_ids(architecture);
    }
}

fn check_clock_ids(architecture: GuestArchitecture) {
    let fixture = Fixture::new();
    let mut runtime = fixture
        .runtime(architecture, fixture.thread)
        .with_clock(Arc::new(FixedClock));
    for clock in 0..=11_u64 {
        let output = 32 + clock * 16;
        let expected = match clock {
            2 | 3 => LinuxResult::Error(hl_linux::Errno::ENOSYS),
            _ => LinuxResult::Value(0),
        };
        assert_eq!(
            runtime.handle(Fixture::operation("clock_gettime"), [clock, output, 0, 0, 0, 0]),
            expected,
            "clock_gettime clock={clock} architecture={architecture:?}",
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_getres"), [clock, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0),
            "clock_getres clock={clock} architecture={architecture:?}",
        );
    }
    for clock in [u64::MAX, 12, 0x7fff] {
        assert_eq!(
            runtime.handle(Fixture::operation("clock_gettime"), [clock, 32, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_getres"), [clock, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
    }
    for (clock, expected) in [
        (-6_i32, LinuxResult::Error(hl_linux::Errno::ENOSYS)),
        (-2, LinuxResult::Error(hl_linux::Errno::ENOSYS)),
    ] {
        assert_eq!(
            runtime.handle(Fixture::operation("clock_gettime"), [clock as u64, 32, 0, 0, 0, 0]),
            expected,
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_getres"), [clock as u64, 32, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn clock_timeline_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock));
        for clock in [0, 5, 8, 11] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_gettime"), [clock, 32, 0, 0, 0, 0]),
                LinuxResult::Value(0),
            );
            assert_eq!(i64::from_le_bytes(fixture.memory.get(32, 8).try_into().unwrap()), 7);
        }
        for clock in [1, 4, 6, 7, 9, 10] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_gettime"), [clock, 32, 0, 0, 0, 0]),
                LinuxResult::Value(0),
            );
            assert_eq!(i64::from_le_bytes(fixture.memory.get(32, 8).try_into().unwrap()), 2);
        }
    }
}

#[test]
fn clock_resolution_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        for (clock, expected) in [(0, 1_i64), (5, 1_000_000), (6, 1_000_000), (11, 1)] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_getres"), [clock, 32, 0, 0, 0, 0]),
                LinuxResult::Value(0),
            );
            assert_eq!(
                i64::from_le_bytes(fixture.memory.get(40, 8).try_into().unwrap()),
                expected
            );
        }
    }
}

#[test]
fn clock_settime_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        for clock in [0, 11] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_settime"), [clock, 1, 0, 0, 0, 0]),
                LinuxResult::Error(hl_linux::Errno::EPERM),
            );
        }
        for clock in [1, 10, 12, u64::MAX] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_settime"), [clock, 1, 0, 0, 0, 0]),
                LinuxResult::Error(hl_linux::Errno::EINVAL),
            );
        }
    }
}

#[test]
fn adjtimex_queries_and_validation_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock));

        assert_eq!(
            runtime.handle(Fixture::operation("adjtimex"), [32, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            i64::from_le_bytes(fixture.memory.get(120, 8).try_into().unwrap()),
            10_000,
        );
        assert_eq!(i64::from_le_bytes(fixture.memory.get(104, 8).try_into().unwrap()), 7);

        assert_eq!(
            runtime.handle(Fixture::operation("clock_adjtime"), [0, 32, 0, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_adjtime"), [0x7fff, u64::MAX, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );

        fixture.memory.put(32, &1_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("adjtimex"), [32, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("adjtimex"), [u64::MAX, 0, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT),
        );
    }
}

#[test]
fn cpu_clocks() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_clock(Arc::new(FixedClock))
            .with_cpu_clock(Arc::new(FixedCpuClock));
        for (clock, output, expected) in [(2, 32, (13, 17)), (3, 64, (19, 23))] {
            assert_eq!(
                runtime.handle(Fixture::operation("clock_gettime"), [clock, output, 0, 0, 0, 0]),
                LinuxResult::Value(0),
            );
            let bytes = fixture.memory.get(usize::try_from(output).unwrap(), 16);
            assert_eq!(i64::from_le_bytes(bytes[..8].try_into().unwrap()), expected.0);
            assert_eq!(i64::from_le_bytes(bytes[8..].try_into().unwrap()), expected.1);
        }
        assert_eq!(
            runtime.handle(Fixture::operation("times"), [96, 0, 0, 0, 0, 0]),
            LinuxResult::Value(200),
        );
        let bytes = fixture.memory.get(96, 32);
        let ticks = bytes
            .chunks_exact(8)
            .map(|value| i64::from_le_bytes(value.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(ticks, [1300, 0, 0, 0]);
    }
}

#[test]
fn prctl_ignores_registers_unused_by_linux() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("prctl"), [1, 9, u64::MAX, 17, 19, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("prctl"), [2, 32, u64::MAX, 17, 19, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(u32::from_le_bytes(fixture.memory.get(32, 4).try_into().unwrap()), 9);
    }
}

#[test]
fn prctl_machine_check_and_perf_controls() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let prctl = |runtime: &mut RuntimeProcessSyscalls<Memory>, arguments| {
            runtime.handle(Fixture::operation("prctl"), arguments)
        };
        assert_eq!(prctl(&mut runtime, [34, 0, 0, 0, 0, 0]), LinuxResult::Value(2));
        assert_eq!(
            prctl(&mut runtime, [34, 1, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(prctl(&mut runtime, [33, 1, 1, 0, 0, 0]), LinuxResult::Value(0));
        assert_eq!(prctl(&mut runtime, [34, 0, 0, 0, 0, 0]), LinuxResult::Value(1));
        assert_eq!(prctl(&mut runtime, [33, 0, 0, 0, 0, 0]), LinuxResult::Value(0));
        assert_eq!(
            prctl(&mut runtime, [33, 7, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(
            prctl(&mut runtime, [33, 1, 7, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        assert_eq!(prctl(&mut runtime, [31, 1, 2, 3, 4, 0]), LinuxResult::Value(0));
        assert_eq!(prctl(&mut runtime, [32, 1, 2, 3, 4, 0]), LinuxResult::Value(0));
        assert_eq!(
            prctl(&mut runtime, [35, 99, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
    }
}

#[test]
fn injected_remaining_time() {
    let fixture = Fixture::new();
    fixture
        .memory
        .put(16, &[2, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0]);
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64, fixture.thread)
        .with_sleep_port(Arc::new(InterruptedSleep));
    assert_eq!(
        runtime.handle(Fixture::operation("nanosleep"), [16, 64, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINTR)
    );
    assert_eq!(
        &fixture.memory.0.lock().unwrap()[64..80],
        &[1, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0],
    );
}

#[test]
fn clock_sleep_probes_remainder_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        fixture.memory.put(16, &[0; 16]);
        let mut runtime = fixture
            .runtime(architecture, fixture.thread)
            .with_sleep_port(Arc::new(InterruptedSleep));
        assert_eq!(
            runtime.handle(Fixture::operation("clock_nanosleep"), [1, 0, 16, 252, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("nanosleep"), [16, 252, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EFAULT)
        );
        assert_eq!(
            runtime.handle(Fixture::operation("clock_nanosleep"), [1, 1, 16, 252, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINTR)
        );
    }
}

#[test]
fn alarm_previous_rounds_up_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let clock = Arc::new(AlarmClock::default());
        let alarms = crate::AlarmRegistry::new(fixture.tasks.clone(), clock.clone());
        let mut runtime = fixture.runtime(architecture, fixture.thread).with_alarms(alarms);
        assert_eq!(
            runtime.handle(Fixture::operation("alarm"), [100, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0)
        );
        *clock.0.lock().unwrap() = 500_000_000;
        assert_eq!(
            runtime.handle(Fixture::operation("alarm"), [50, 0, 0, 0, 0, 0]),
            LinuxResult::Value(100)
        );
        *clock.0.lock().unwrap() = 1_600_000_000;
        assert_eq!(
            runtime.handle(Fixture::operation("alarm"), [0, 0, 0, 0, 0, 0]),
            LinuxResult::Value(49)
        );
    }
}

#[test]
fn rlimit_owned_state() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("getpgid"), [0, 0, 0, 0, 0, 0]),
        LinuxResult::Value(1),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("getsid"), [0, 0, 0, 0, 0, 0]),
        LinuxResult::Value(1),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("getrlimit"), [7, 32, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        &fixture.memory.0.lock().unwrap()[32..48],
        &[64, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0],
    );
    fixture
        .memory
        .put(64, &[32, 0, 0, 0, 0, 0, 0, 0, 96, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        runtime.handle(Fixture::operation("setrlimit"), [7, 64, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture.memory.put(96, b"worker\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(
        runtime.handle(Fixture::operation("prctl"), [15, 96, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("prctl"), [16, 128, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(&fixture.memory.0.lock().unwrap()[128..134], b"worker");
    assert_eq!(
        runtime.handle(Fixture::operation("setsid"), [0; 6]),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
}

#[test]
fn prlimit_ordering() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    fixture
        .memory
        .put(160, &[16, 0, 0, 0, 0, 0, 0, 0, 80, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        runtime.handle(Fixture::operation("prlimit64"), [0, 7, 160, 252, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("getrlimit"), [7, 192, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        &fixture.memory.get(192, 16),
        &[16, 0, 0, 0, 0, 0, 0, 0, 80, 0, 0, 0, 0, 0, 0, 0,]
    );
    assert_eq!(
        runtime.handle(Fixture::operation("prlimit64"), [0, 7, 252, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("getrlimit"), [7, 208, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(fixture.memory.get(208, 16), fixture.memory.get(192, 16));
}

#[test]
fn prlimit_permissions() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = plan.process();
    fixture.tasks.commit_fork_process(plan).unwrap();
    let user = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(fixture.process, user).unwrap();
    let other = ProcessCredentials::new(2000, 2000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(child, other).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64, fixture.thread);
    assert_eq!(
        runtime.handle(
            Fixture::operation("prlimit64"),
            [child.number() as u64, 7, 0, 128, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("prlimit64"), [u32::MAX as u64, 7, 0, 128, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::ESRCH),
    );
}

#[test]
fn hard_limit_raise() {
    let fixture = Fixture::new();
    let user = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(fixture.process, user).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    fixture
        .memory
        .put(160, &[32, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        runtime.handle(Fixture::operation("setrlimit"), [7, 160, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    fixture
        .memory
        .put(176, &[32, 0, 0, 0, 0, 0, 0, 0, 80, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        runtime.handle(Fixture::operation("setrlimit"), [7, 176, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
}

#[test]
fn group_errors() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut parent = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            parent.handle(Fixture::operation("setpgid"), [0, u64::MAX, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL),
        );
        for name in ["setpgid", "getpgid", "getsid"] {
            assert_eq!(
                parent.handle(Fixture::operation(name), [u64::MAX, 0, 0, 0, 0, 0]),
                LinuxResult::Error(hl_linux::Errno::ESRCH),
            );
        }
        let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
        let (child_process, child_thread) = fixture.tasks.commit_fork_process(plan).unwrap();
        assert_eq!(
            parent.handle(
                Fixture::operation("setpgid"),
                [child_process.number() as u64, child_process.number() as u64, 0, 0, 0, 0,]
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(
            parent.handle(
                Fixture::operation("getpgid"),
                [child_process.number() as u64, 0, 0, 0, 0, 0]
            ),
            LinuxResult::Value(child_process.number() as u64),
        );
        fixture.tasks.mark_exec(child_process).unwrap();
        assert_eq!(
            parent.handle(
                Fixture::operation("setpgid"),
                [child_process.number() as u64, 0, 0, 0, 0, 0]
            ),
            LinuxResult::Error(hl_linux::Errno::EACCES),
        );
        let mut child = fixture.runtime_for(architecture, child_process, child_thread);
        assert_eq!(
            child.handle(Fixture::operation("setsid"), [0; 6]),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
        let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
        let (session_process, session_thread) = fixture.tasks.commit_fork_process(plan).unwrap();
        let mut session = fixture.runtime_for(architecture, session_process, session_thread);
        assert_eq!(
            session.handle(Fixture::operation("setsid"), [0; 6]),
            LinuxResult::Value(session_process.number() as u64),
        );
        assert_eq!(
            parent.handle(
                Fixture::operation("setpgid"),
                [
                    session_process.number() as u64,
                    fixture.process.number() as u64,
                    0,
                    0,
                    0,
                    0,
                ]
            ),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
        assert_eq!(
            session.handle(Fixture::operation("setpgid"), [0; 6]),
            LinuxResult::Error(hl_linux::Errno::EPERM),
        );
    }
}

#[test]
fn wait4_consuming_child() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (child, _) = fixture.tasks.commit_fork_process(plan).unwrap();
    fixture.tasks.exit_process(child, hl_task::ExitStatus::Code(7)).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("wait4"), [child.number() as u64, 1000, 1, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("wait4"), [child.number() as u64, 32, 1, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::ECHILD),
    );
}

#[test]
fn bad_usage_reaps() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (child, _) = fixture.tasks.commit_fork_process(plan).unwrap();
    fixture.tasks.charge_cpu(child, 37).unwrap();
    fixture.tasks.exit_process(child, hl_task::ExitStatus::Code(0)).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("wait4"), [child.number() as u64, 32, 1, 1000, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        fixture.tasks.cpu_usage(fixture.process).unwrap().children_nanoseconds,
        37
    );
    assert_eq!(
        runtime.handle(Fixture::operation("wait4"), [child.number() as u64, 32, 1, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::ECHILD),
    );
}

#[test]
fn nowait_survives_fault() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (child, _) = fixture.tasks.commit_fork_process(plan).unwrap();
    fixture.tasks.exit_process(child, hl_task::ExitStatus::Code(9)).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    let options = 0x0100_0000 | 4;
    assert_eq!(
        runtime.handle(
            Fixture::operation("waitid"),
            [1, child.number() as u64, 1000, options, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("waitid"),
            [1, child.number() as u64, 32, options, 0, 0],
        ),
        LinuxResult::Value(0),
    );
    assert_eq!(
        u32::from_le_bytes(fixture.memory.get(48, 4).try_into().unwrap()),
        child.number()
    );
}

#[test]
fn waitid_usage_reaps() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let (child, _) = fixture.tasks.commit_fork_process(plan).unwrap();
    fixture.tasks.charge_cpu(child, 41).unwrap();
    fixture.tasks.exit_process(child, hl_task::ExitStatus::Code(5)).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("waitid"), [1, child.number() as u64, 32, 4, 1000, 0],),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        u32::from_le_bytes(fixture.memory.get(48, 4).try_into().unwrap()),
        child.number()
    );
    assert_eq!(
        fixture.tasks.cpu_usage(fixture.process).unwrap().children_nanoseconds,
        41
    );
    assert_eq!(
        runtime.handle(Fixture::operation("waitid"), [1, child.number() as u64, 32, 4, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ECHILD),
    );
}

#[test]
fn wait4_signal_restart_isas() {
    for (architecture, flags, expected) in [
        (
            GuestArchitecture::Aarch64,
            0x1000_0000,
            LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt),
        ),
        (
            GuestArchitecture::X86_64,
            0x1000_0000,
            LinuxResult::Restart(hl_linux::RestartKind::NoInterrupt),
        ),
        (
            GuestArchitecture::Aarch64,
            0,
            LinuxResult::Error(hl_linux::Errno::EINTR),
        ),
        (GuestArchitecture::X86_64, 0, LinuxResult::Error(hl_linux::Errno::EINTR)),
    ] {
        let fixture = Fixture::new();
        let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
        let (child, _) = fixture.tasks.commit_fork_process(plan).unwrap();
        let signal = SignalNumber::new(10).unwrap();
        fixture
            .tasks
            .set_action(
                fixture.process,
                signal,
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    flags,
                    restorer: 0,
                    mask: SignalMask::from_bits(0),
                },
            )
            .unwrap();
        fixture
            .tasks
            .enqueue_signal(PendingTarget::Thread(fixture.thread), SignalInfo::bare(signal))
            .unwrap();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        assert_eq!(
            runtime.handle(Fixture::operation("wait4"), [child.number() as u64, 32, 0, 0, 0, 0]),
            expected
        );
        assert!(fixture.tasks.prepare_forced_delivery(fixture.thread).is_some());
    }
}

#[test]
fn kill_exact_thread() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_clone_thread(fixture.thread).unwrap();
    let worker = fixture.tasks.commit_clone_thread(plan).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(
            Fixture::operation("kill"),
            [fixture.process.number() as u64, 0, 0, 0, 0, 0],
        ),
        LinuxResult::Value(0),
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("tgkill"),
            [fixture.process.number() as u64, worker.number() as u64, 10, 0, 0, 0,],
        ),
        LinuxResult::Value(0),
    );
    assert_eq!(fixture.tasks.dequeue_signal(fixture.thread).unwrap(), None,);
    assert_eq!(
        fixture.tasks.dequeue_signal(worker).unwrap().unwrap().0.signal.get(),
        10,
    );
    assert_eq!(
        runtime.handle(Fixture::operation("tgkill"), [999, worker.number() as u64, 10, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::ESRCH),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("tkill"), [999, 10, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ESRCH),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("tkill"), [0, 65, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("tgkill"),
            [u64::MAX, worker.number() as u64, 65, 0, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("kill"),
            [fixture.process.number() as u64, 65, 0, 0, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
}

#[test]
fn sigaltstack_rejection_preserves_state() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64, fixture.thread);
    let mut stack = [0_u8; 24];
    stack[..8].copy_from_slice(&64_u64.to_le_bytes());
    stack[16..24].copy_from_slice(&8192_u64.to_le_bytes());
    fixture.memory.put(32, &stack);
    assert_eq!(
        runtime.handle(Fixture::operation("sigaltstack"), [32, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );

    stack[16..24].copy_from_slice(&1_u64.to_le_bytes());
    fixture.memory.put(32, &stack);
    assert_eq!(
        runtime.handle(Fixture::operation("sigaltstack"), [32, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOMEM),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("sigaltstack"), [0, 96, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(u64::from_le_bytes(fixture.memory.get(96, 8).try_into().unwrap()), 64);
    assert_eq!(u64::from_le_bytes(fixture.memory.get(112, 8).try_into().unwrap()), 8192,);
}

#[test]
fn queued_signal_preserves_info() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        let mut info = [0_u8; 128];
        info[..4].copy_from_slice(&9_i32.to_le_bytes());
        info[4..8].copy_from_slice(&3_i32.to_le_bytes());
        info[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
        info[16..20].copy_from_slice(&17_u32.to_le_bytes());
        info[20..24].copy_from_slice(&19_u32.to_le_bytes());
        info[24..32].copy_from_slice(&23_u64.to_le_bytes());
        fixture.memory.put(32, &info);
        assert_eq!(
            runtime.handle(
                Fixture::operation("rt_sigqueueinfo"),
                [fixture.process.number() as u64, 35, 32, 0, 0, 0],
            ),
            LinuxResult::Value(0),
        );
        let delivered = fixture.tasks.dequeue_signal(fixture.thread).unwrap().unwrap().0;
        assert_eq!(delivered.signal.get(), 35);
        assert_eq!(delivered.code, -1);
        assert_eq!(delivered.error, 3);
        assert_eq!(
            (delivered.sender_process, delivered.sender_user, delivered.value),
            (17, 19, 23)
        );
        assert_eq!(
            runtime.handle(
                Fixture::operation("rt_sigqueueinfo"),
                [fixture.process.number() as u64, 0, 32, 0, 0, 0],
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(fixture.tasks.dequeue_signal(fixture.thread).unwrap(), None);
    }
}

#[test]
fn queued_signal_validation_order() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = plan.process();
    fixture.tasks.commit_fork_process(plan).unwrap();
    let mut runtime = fixture.runtime(GuestArchitecture::X86_64, fixture.thread);
    let mut info = [0_u8; 128];
    info[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    fixture.memory.put(32, &info);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigqueueinfo"), [0, 35, 256, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigqueueinfo"), [0, 35, 32, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    info[8..12].copy_from_slice(&0_i32.to_le_bytes());
    fixture.memory.put(32, &info);
    assert_eq!(
        runtime.handle(Fixture::operation("rt_sigqueueinfo"), [999, 35, 32, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
    info[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    fixture.memory.put(32, &info);
    let sender = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(fixture.process, sender).unwrap();
    let target = ProcessCredentials::new(2000, 2000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(child, target).unwrap();
    assert_eq!(
        runtime.handle(
            Fixture::operation("rt_sigqueueinfo"),
            [child.number() as u64, 35, 32, 0, 0, 0],
        ),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
}

#[test]
fn absent_state_unchanged() {
    let fixture = Fixture::new();
    let before = fixture.tasks.snapshot();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    assert_eq!(
        runtime.handle(Fixture::operation("execve"), [1000, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    fixture.memory.put(32, b"/bin/app\0");
    fixture.memory.put(64, &32_u64.to_le_bytes());
    fixture.memory.put(72, &0_u64.to_le_bytes());
    fixture.memory.put(80, &0_u64.to_le_bytes());
    assert_eq!(
        runtime.handle(Fixture::operation("execve"), [32, 64, 80, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("clone3"), [1000, 8, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    fixture.memory.put(96, &[0; 88]);
    assert_eq!(
        runtime.handle(Fixture::operation("clone3"), [96, 88, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("clone"), [0xffff_0000_0000_0000, 0, 0, 0, 0, 0],),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(Fixture::operation("fork"), [0; 6]),
        LinuxResult::Error(hl_linux::Errno::ENOSYS),
    );
    assert_eq!(fixture.tasks.snapshot(), before);
}

#[test]
fn capability_versions() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime(architecture, fixture.thread);
        for (version, words) in [(0x1998_0330_u32, 1_usize), (0x2007_1026, 2), (0x2008_0522, 2)] {
            fixture.memory.put(8, &version.to_le_bytes());
            fixture.memory.put(12, &0_i32.to_le_bytes());
            assert_eq!(
                runtime.handle(Fixture::operation("capget"), [8, 32, 0, 0, 0, 0]),
                LinuxResult::Value(0)
            );
            let data = fixture.memory.get(32, words * 12);
            let container = hl_task::CapabilitySets::CONTAINER as u32;
            assert_eq!(u32::from_le_bytes(data[..4].try_into().unwrap()), container);
            assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), container);
        }
        fixture.memory.put(8, &0_u32.to_le_bytes());
        assert_eq!(
            runtime.handle(Fixture::operation("capget"), [8, 32, 0, 0, 0, 0]),
            LinuxResult::Error(hl_linux::Errno::EINVAL)
        );
        assert_eq!(fixture.memory.get(8, 4), 0x2008_0522_u32.to_le_bytes());
    }
}

#[test]
fn capability_transaction() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(GuestArchitecture::Aarch64, fixture.thread);
    fixture.memory.put(8, &0x2008_0522_u32.to_le_bytes());
    fixture.memory.put(12, &0_i32.to_le_bytes());
    let mut data = [0_u8; 24];
    data[..4].copy_from_slice(&1_u32.to_le_bytes());
    data[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fixture.memory.put(32, &data);
    assert_eq!(
        runtime.handle(Fixture::operation("capset"), [8, 32, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    fixture.memory.put(32, &[0; 24]);
    assert_eq!(
        runtime.handle(Fixture::operation("capget"), [8, 32, 0, 0, 0, 0]),
        LinuxResult::Value(0)
    );
    assert_eq!(&fixture.memory.get(32, 8), &data[..8]);
    let before = fixture.tasks.snapshot();
    assert_eq!(
        runtime.handle(Fixture::operation("capset"), [8, 250, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::EFAULT)
    );
    assert_eq!(fixture.tasks.snapshot(), before);
}

#[test]
fn pidfd_lifecycle() {
    let fixture = Fixture::new();
    let descriptors = Arc::new(hl_descriptor::DescriptorTable::new(5).unwrap());
    let handles = Arc::new(crate::ProcessHandleRegistry::new());
    let source = descriptors
        .install(
            0,
            crate::ProcessHandleRegistry::create(fixture.process),
            hl_descriptor::DescriptorFlags::from_bits(hl_descriptor::DescriptorFlags::CLOSE_ON_EXEC),
        )
        .unwrap();
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64, fixture.thread)
        .with_process_handles(descriptors.clone(), handles);
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_open"),
            [fixture.process.number() as u64, 1, 0, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EINVAL)
    );
    assert_eq!(
        runtime.handle(Fixture::operation("pidfd_open"), [9999, 0, 0, 0, 0, 0]),
        LinuxResult::Error(hl_linux::Errno::ESRCH)
    );
    let descriptor = match runtime.handle(
        Fixture::operation("pidfd_open"),
        [fixture.process.number() as u64, 0, 0, 0, 0, 0],
    ) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("unexpected {result:?}"),
    };
    assert_ne!(
        descriptors.snapshot(descriptor).unwrap().flags.bits() & hl_descriptor::DescriptorFlags::CLOSE_ON_EXEC,
        0
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_getfd"),
            [descriptor as u64, source as u64, 1, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EINVAL)
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_getfd"),
            [source as u64, source as u64, 0, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EBADF)
    );
    let transferred = match runtime.handle(
        Fixture::operation("pidfd_getfd"),
        [descriptor as u64, source as u64, 0, 0, 0, 0],
    ) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("unexpected {result:?}"),
    };
    assert_eq!(
        descriptors.snapshot(transferred).unwrap().description_identity,
        descriptors.snapshot(source).unwrap().description_identity
    );
    assert_eq!(descriptors.snapshot(transferred).unwrap().flags.bits(), 0);
    let nonblocking = match runtime.handle(
        Fixture::operation("pidfd_open"),
        [fixture.process.number() as u64, 0x800, 0, 0, 0, 0],
    ) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("unexpected {result:?}"),
    };
    assert_ne!(
        descriptors.pin(nonblocking).unwrap().status().bits() & hl_descriptor::StatusFlags::NONBLOCKING,
        0
    );
    assert_ne!(
        descriptors.snapshot(nonblocking).unwrap().flags.bits() & hl_descriptor::DescriptorFlags::CLOSE_ON_EXEC,
        0
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 0, 0, 0, 0, 0]
        ),
        LinuxResult::Value(0)
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 0, 1, 0, 0, 0]
        ),
        LinuxResult::Value(0)
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 0, 0, 1, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EINVAL)
    );
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 35, 250, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EFAULT)
    );
    let mut information = [0_u8; 128];
    information[0..4].copy_from_slice(&35_u32.to_le_bytes());
    information[4..8].copy_from_slice(&3_i32.to_le_bytes());
    information[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    information[16..20].copy_from_slice(&17_u32.to_le_bytes());
    information[20..24].copy_from_slice(&19_u32.to_le_bytes());
    information[24..32].copy_from_slice(&23_u64.to_le_bytes());
    fixture.memory.put(32, &information);
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 35, 32, 0, 0, 0]
        ),
        LinuxResult::Value(0)
    );
    let delivered = fixture.tasks.dequeue_signal(fixture.thread).unwrap().unwrap().0;
    assert_eq!(
        (
            delivered.signal.get(),
            delivered.error,
            delivered.code,
            delivered.sender_process,
            delivered.sender_user,
            delivered.value,
        ),
        (35, 3, -1, 17, 19, 23)
    );
    descriptors.close(descriptor).unwrap();
    assert_eq!(
        runtime.handle(
            Fixture::operation("pidfd_send_signal"),
            [descriptor as u64, 0, 0, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EBADF)
    );
}

#[test]
fn pidfd_getfd_permissions() {
    let fixture = Fixture::new();
    let plan = fixture.tasks.begin_fork_process(fixture.thread).unwrap();
    let child = plan.process();
    let (_, child_thread) = fixture.tasks.commit_fork_process(plan).unwrap();
    let parent_files = Arc::new(hl_descriptor::DescriptorTable::new(4).unwrap());
    let child_files = Arc::new(hl_descriptor::DescriptorTable::new(2).unwrap());
    let handles = Arc::new(crate::ProcessHandleRegistry::new());
    let child_source = child_files
        .install(
            0,
            crate::ProcessHandleRegistry::create(child),
            hl_descriptor::DescriptorFlags::default(),
        )
        .unwrap();
    let _child_runtime = fixture
        .runtime_for(GuestArchitecture::Aarch64, child, child_thread)
        .with_process_handles(child_files.clone(), handles.clone());
    let mut parent_runtime = fixture
        .runtime(GuestArchitecture::Aarch64, fixture.thread)
        .with_process_handles(parent_files.clone(), handles);
    let pidfd = match parent_runtime.handle(Fixture::operation("pidfd_open"), [child.number() as u64, 0, 0, 0, 0, 0]) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("unexpected {result:?}"),
    };

    let unrelated = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    fixture.tasks.replace_credentials(child, unrelated).unwrap();
    assert_eq!(
        parent_runtime.handle(
            Fixture::operation("pidfd_getfd"),
            [pidfd as u64, child_source as u64, 0, 0, 0, 0]
        ),
        LinuxResult::Error(hl_linux::Errno::EPERM)
    );

    let mut privileged = fixture.tasks.credentials(fixture.process).unwrap();
    privileged.capabilities.permitted |= 1_u64 << 19;
    fixture.tasks.replace_credentials(fixture.process, privileged).unwrap();
    let transferred = match parent_runtime.handle(
        Fixture::operation("pidfd_getfd"),
        [pidfd as u64, child_source as u64, 0, 0, 0, 0],
    ) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("unexpected {result:?}"),
    };
    assert_eq!(
        parent_files.snapshot(transferred).unwrap().description_identity,
        child_files.snapshot(child_source).unwrap().description_identity
    );
    assert_eq!(parent_files.snapshot(transferred).unwrap().flags.bits(), 0);
}
