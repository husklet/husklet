use hl_execution::{EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionSnapshot};
use hl_linux::ClonePlan;
use hl_linux::{GuestAccess, GuestMemory, LinuxResult};
use hl_task::{TaskError, TaskRegistry, ThreadId};
use std::sync::Arc;

use super::port::{RuntimeThreadError, RuntimeThreadPort};

const VM: u64 = 0x0000_0100;
const FS: u64 = 0x0000_0200;
const FILES: u64 = 0x0000_0400;
const SIGHAND: u64 = 0x0000_0800;
const THREAD: u64 = 0x0001_0000;
const SYSVSEM: u64 = 0x0004_0000;
const SETTLS: u64 = 0x0008_0000;
const PARENT_SETTID: u64 = 0x0010_0000;
const CHILD_CLEARTID: u64 = 0x0020_0000;
const CHILD_SETTID: u64 = 0x0100_0000;
const REQUIRED: u64 = VM | FS | FILES | SIGHAND | THREAD | SYSVSEM;
const OPTIONAL: u64 = SETTLS | PARENT_SETTID | CHILD_CLEARTID | CHILD_SETTID;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    Fault,
    Again,
    NoMemory,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Plan {
    pub stack: u64,
    pub parent_tid: Option<u64>,
    pub child_tid: Option<u64>,
    pub clear_tid: Option<u64>,
    pub tls: Option<u64>,
}

pub trait ContextPort: Send + Sync {
    /// Prepares the child context from the calling thread's current resources.
    fn prepare(&self, source: ThreadId, thread: ThreadId) -> Result<(), Error>;
    fn rollback(&self, thread: ThreadId);
}

pub trait TrapPort: Send + Sync {
    fn clone(&self, cpu: &ExecutionCpuSnapshot, plan: ClonePlan) -> LinuxResult;
}

pub struct Runtime<M: GuestMemory> {
    tasks: Arc<TaskRegistry>,
    runnable: Arc<dyn RuntimeThreadPort>,
    contexts: Arc<dyn ContextPort>,
    memory: M,
}

struct Copyout {
    address: u64,
    previous: [u8; 4],
}

impl<M: GuestMemory> Runtime<M> {
    pub fn new(
        tasks: Arc<TaskRegistry>,
        runnable: Arc<dyn RuntimeThreadPort>,
        contexts: Arc<dyn ContextPort>,
        memory: M,
    ) -> Self {
        Self {
            tasks,
            runnable,
            contexts,
            memory,
        }
    }

    pub fn spawn(&self, source: ThreadId, cpu: &ExecutionCpuSnapshot, linux: ClonePlan) -> Result<ThreadId, Error> {
        let plan = Plan::from_linux(linux)?;
        let copyouts = self.copyouts(plan)?;
        let task = self.tasks.begin_clone_thread(source).map_err(Self::task_error)?;
        let thread = task.thread();
        if let Err(error) = self.contexts.prepare(source, thread) {
            let _ = self.tasks.rollback_clone_thread(task);
            return Err(error);
        }
        let runnable = match self.runnable.stage(thread, plan.child(cpu)) {
            Ok(value) => value,
            Err(error) => {
                self.contexts.rollback(thread);
                let _ = self.tasks.rollback_clone_thread(task);
                return Err(Self::runnable_error(error));
            }
        };
        if let Some(address) = plan.clear_tid {
            if self.tasks.stage_clear_tid(&task, address).is_err() {
                drop(runnable);
                self.contexts.rollback(thread);
                let _ = self.tasks.rollback_clone_thread(task);
                return Err(Error::Failed);
            }
        }
        if self.write_tid(thread, &copyouts).is_err() {
            self.restore(&copyouts);
            drop(runnable);
            self.contexts.rollback(thread);
            let _ = self.tasks.rollback_clone_thread(task);
            return Err(Error::Fault);
        }
        if self.tasks.commit_clone_thread(task).is_err() {
            self.restore(&copyouts);
            drop(runnable);
            self.contexts.rollback(thread);
            return Err(Error::Failed);
        }
        runnable.publish();
        Ok(thread)
    }

    fn copyouts(&self, plan: Plan) -> Result<Vec<Copyout>, Error> {
        let mut copyouts = Vec::with_capacity(2);
        for address in [plan.parent_tid, plan.child_tid].into_iter().flatten() {
            if self.memory.probe(address, 4, GuestAccess::Write) != Ok(4) {
                return Err(Error::Fault);
            }
            let mut previous = [0; 4];
            if self.memory.read(address, &mut previous) != Ok(4) {
                return Err(Error::Fault);
            }
            copyouts.push(Copyout { address, previous });
        }
        Ok(copyouts)
    }

    fn write_tid(&self, thread: ThreadId, copyouts: &[Copyout]) -> Result<(), ()> {
        let tid = thread.number().to_le_bytes();
        for output in copyouts {
            if self.memory.write(output.address, &tid) != Ok(4) {
                return Err(());
            }
        }
        Ok(())
    }

    fn restore(&self, copyouts: &[Copyout]) {
        for output in copyouts.iter().rev() {
            let _ = self.memory.write(output.address, &output.previous);
        }
    }

    const fn task_error(error: TaskError) -> Error {
        match error {
            TaskError::ThreadLimit => Error::Again,
            _ => Error::Failed,
        }
    }

    const fn runnable_error(error: RuntimeThreadError) -> Error {
        match error {
            RuntimeThreadError::Capacity => Error::Again,
            RuntimeThreadError::Invalid => Error::Invalid,
            RuntimeThreadError::Duplicate | RuntimeThreadError::Missing => Error::Failed,
        }
    }
}

pub struct Trap<M: GuestMemory> {
    runtime: Arc<Runtime<M>>,
    source: ThreadId,
}

impl<M: GuestMemory> Trap<M> {
    pub fn new(runtime: Arc<Runtime<M>>, source: ThreadId) -> Self {
        Self { runtime, source }
    }

    fn errno(error: Error) -> hl_linux::Errno {
        match error {
            Error::Invalid => hl_linux::Errno::EINVAL,
            Error::Fault => hl_linux::Errno::EFAULT,
            Error::Again => hl_linux::Errno::EAGAIN,
            Error::NoMemory => hl_linux::Errno::ENOMEM,
            Error::Failed => hl_linux::Errno::EIO,
        }
    }
}

impl<M: GuestMemory + Send + Sync> TrapPort for Trap<M> {
    fn clone(&self, cpu: &ExecutionCpuSnapshot, plan: ClonePlan) -> LinuxResult {
        match self.runtime.spawn(self.source, cpu, plan) {
            Ok(thread) => {
                hl_log::hl_info!(
                    hl_log::tag::TASK,
                    "thread spawned source={} thread={} flags={:#x} stack={:#x}",
                    self.source.number(),
                    thread.number(),
                    plan.flags,
                    plan.stack,
                );
                LinuxResult::Value(u64::from(thread.number()))
            }
            Err(error) => {
                hl_log::hl_debug!(
                    hl_log::tag::TASK,
                    "thread spawn refused source={} flags={:#x} error={error:?}",
                    self.source.number(),
                    plan.flags,
                );
                LinuxResult::Error(Self::errno(error))
            }
        }
    }
}

impl Plan {
    pub fn from_linux(plan: ClonePlan) -> Result<Self, Error> {
        if plan.flags & REQUIRED != REQUIRED
            || plan.flags & !(REQUIRED | OPTIONAL) != 0
            || plan.exit_signal != 0
            || plan.stack == 0
            || plan.set_tid != 0
            || plan.set_tid_count != 0
            || plan.cgroup != 0
        {
            return Err(Error::Invalid);
        }
        let stack = if plan.stack_size == 0 {
            plan.stack
        } else {
            plan.stack.checked_add(plan.stack_size).ok_or(Error::Invalid)?
        };
        Ok(Self {
            stack,
            parent_tid: (plan.flags & PARENT_SETTID != 0).then_some(plan.parent_tid),
            child_tid: (plan.flags & CHILD_SETTID != 0).then_some(plan.child_tid),
            clear_tid: (plan.flags & CHILD_CLEARTID != 0).then_some(plan.child_tid),
            tls: (plan.flags & SETTLS != 0).then_some(plan.tls),
        })
    }

    #[must_use]
    pub fn child(&self, parent: &ExecutionCpuSnapshot) -> ExecutionSnapshot {
        let mut cpu = parent.clone();
        match &mut cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => {
                cpu.registers[0] = 0;
                cpu.sp = self.stack;
                if let Some(tls) = self.tls {
                    cpu.tls = tls;
                }
                cpu.clear_exclusive_reservation();
            }
            ExecutionCpuSnapshot::X86_64(cpu) => {
                cpu.registers[0] = 0;
                cpu.registers[4] = self.stack;
                if let Some(tls) = self.tls {
                    cpu.fs_base = tls;
                }
            }
        }
        ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_execution::{Aarch64CpuState, CpuState};
    use hl_task::{InterruptSink, ProcessCredentials, ProcessLimits, RegistryConfig};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Interrupt(AtomicBool);

    impl InterruptSink for Interrupt {
        fn set_interrupted(&self, interrupted: bool) {
            self.0.store(interrupted, Ordering::Release);
        }
    }

    #[derive(Clone, Default)]
    struct Memory {
        bytes: Arc<Mutex<BTreeMap<u64, [u8; 4]>>>,
        fail: Arc<Mutex<Option<u64>>>,
    }

    impl GuestMemory for Memory {
        fn probe(&self, address: u64, length: usize, _: GuestAccess) -> Result<usize, hl_linux::GuestFault> {
            (length == 4 && self.bytes.lock().unwrap().contains_key(&address))
                .then_some(4)
                .ok_or(hl_linux::GuestFault {
                    address,
                    access: GuestAccess::Write,
                })
        }

        fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, hl_linux::GuestFault> {
            let bytes = self
                .bytes
                .lock()
                .unwrap()
                .get(&address)
                .copied()
                .ok_or(hl_linux::GuestFault {
                    address,
                    access: GuestAccess::Read,
                })?;
            output.copy_from_slice(&bytes);
            Ok(4)
        }

        fn write(&self, address: u64, input: &[u8]) -> Result<usize, hl_linux::GuestFault> {
            if *self.fail.lock().unwrap() == Some(address) {
                return Err(hl_linux::GuestFault {
                    address,
                    access: GuestAccess::Write,
                });
            }
            let value: [u8; 4] = input.try_into().map_err(|_| hl_linux::GuestFault {
                address,
                access: GuestAccess::Write,
            })?;
            *self
                .bytes
                .lock()
                .unwrap()
                .get_mut(&address)
                .ok_or(hl_linux::GuestFault {
                    address,
                    access: GuestAccess::Write,
                })? = value;
            Ok(4)
        }
    }

    #[derive(Default)]
    struct Contexts(Mutex<BTreeMap<ThreadId, ThreadId>>);

    impl ContextPort for Contexts {
        fn prepare(&self, source: ThreadId, thread: ThreadId) -> Result<(), Error> {
            if self.0.lock().unwrap().insert(thread, source).is_some() {
                return Err(Error::Failed);
            }
            Ok(())
        }

        fn rollback(&self, thread: ThreadId) {
            self.0.lock().unwrap().remove(&thread);
        }
    }

    #[derive(Default)]
    struct Runnables {
        snapshots: Arc<Mutex<BTreeMap<ThreadId, ExecutionSnapshot>>>,
        failure: Mutex<Option<RuntimeThreadError>>,
        tasks: Option<Arc<TaskRegistry>>,
        interrupt: Option<Arc<Interrupt>>,
    }

    struct Staged {
        snapshots: Arc<Mutex<BTreeMap<ThreadId, ExecutionSnapshot>>>,
        thread: ThreadId,
        snapshot: Option<ExecutionSnapshot>,
    }

    impl super::super::port::PreparedThread for Staged {
        fn publish(mut self: Box<Self>) {
            self.snapshots
                .lock()
                .unwrap()
                .insert(self.thread, self.snapshot.take().unwrap());
        }
    }

    impl RuntimeThreadPort for Runnables {
        fn stage(
            &self,
            thread: ThreadId,
            snapshot: ExecutionSnapshot,
        ) -> Result<Box<dyn super::super::port::PreparedThread>, RuntimeThreadError> {
            if let Some(error) = *self.failure.lock().unwrap() {
                return Err(error);
            }
            if self.snapshots.lock().unwrap().contains_key(&thread) {
                return Err(RuntimeThreadError::Duplicate);
            }
            if let (Some(tasks), Some(interrupt)) = (&self.tasks, &self.interrupt) {
                let sink: Arc<dyn InterruptSink> = interrupt.clone();
                tasks
                    .register_interrupt(thread, sink)
                    .map_err(|_| RuntimeThreadError::Invalid)?;
            }
            Ok(Box::new(Staged {
                snapshots: Arc::clone(&self.snapshots),
                thread,
                snapshot: Some(snapshot),
            }))
        }

        fn terminate(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
            self.snapshots
                .lock()
                .unwrap()
                .remove(&thread)
                .map(|_| ())
                .ok_or(RuntimeThreadError::Missing)
        }
    }

    fn tasks() -> (Arc<TaskRegistry>, ThreadId) {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let (_, thread) = tasks
            .create_init(
                ProcessCredentials::new(0, 0, &[], 32).unwrap(),
                ProcessLimits::default(),
            )
            .unwrap();
        (tasks, thread)
    }

    fn linux(flags: u64) -> ClonePlan {
        ClonePlan {
            flags,
            stack: 0x8000,
            stack_size: 0,
            parent_tid: 0x1000,
            child_tid: 0x2000,
            tls: 0x3000,
            exit_signal: 0,
            pidfd: 0,
            set_tid: 0,
            set_tid_count: 0,
            cgroup: 0,
        }
    }

    #[test]
    fn pthread_flags_exact() {
        let all = REQUIRED | OPTIONAL;
        let plan = Plan::from_linux(linux(all)).unwrap();
        assert_eq!(
            (plan.parent_tid, plan.child_tid, plan.clear_tid, plan.tls),
            (Some(0x1000), Some(0x2000), Some(0x2000), Some(0x3000))
        );
        let mut clone3 = linux(all);
        clone3.pidfd = clone3.parent_tid;
        assert!(Plan::from_linux(clone3).is_ok());
        assert_eq!(Plan::from_linux(linux(REQUIRED & !VM)), Err(Error::Invalid));
        assert_eq!(Plan::from_linux(linux(REQUIRED | 0x80)), Err(Error::Invalid));
        let mut signaled = linux(REQUIRED);
        signaled.exit_signal = 17;
        assert_eq!(Plan::from_linux(signaled), Err(Error::Invalid));
    }

    #[test]
    fn child_isas() {
        let plan = Plan::from_linux(linux(REQUIRED | SETTLS)).unwrap();
        let mut arm = Aarch64CpuState::default();
        arm.registers[0] = 9;
        let child = plan.child(&ExecutionCpuSnapshot::Aarch64(arm));
        let ExecutionCpuSnapshot::Aarch64(arm) = child.cpu else {
            unreachable!()
        };
        assert_eq!((arm.registers[0], arm.sp, arm.tls), (0, 0x8000, 0x3000));
        let mut x86 = CpuState::default();
        x86.registers[0] = 9;
        let child = plan.child(&ExecutionCpuSnapshot::X86_64(x86));
        let ExecutionCpuSnapshot::X86_64(x86) = child.cpu else {
            unreachable!()
        };
        assert_eq!((x86.registers[0], x86.registers[4], x86.fs_base), (0, 0x8000, 0x3000));
    }

    #[test]
    fn transaction_publishes_last() {
        let (tasks, source) = tasks();
        let memory = Memory::default();
        memory.bytes.lock().unwrap().insert(0x1000, [0xaa; 4]);
        memory.bytes.lock().unwrap().insert(0x2000, [0xbb; 4]);
        let runnable = Arc::new(Runnables::default());
        let contexts = Arc::new(Contexts::default());
        let runtime = Runtime::new(Arc::clone(&tasks), runnable.clone(), contexts.clone(), memory.clone());
        let thread = runtime
            .spawn(
                source,
                &ExecutionCpuSnapshot::Aarch64(Aarch64CpuState::default()),
                linux(REQUIRED | OPTIONAL),
            )
            .unwrap();
        assert_eq!(tasks.clear_tid(thread).unwrap(), Some(0x2000));
        assert_eq!(memory.bytes.lock().unwrap()[&0x1000], thread.number().to_le_bytes());
        assert_eq!(memory.bytes.lock().unwrap()[&0x2000], thread.number().to_le_bytes());
        assert_eq!(contexts.0.lock().unwrap().get(&thread), Some(&source));
        assert!(runnable.snapshots.lock().unwrap().contains_key(&thread));
    }

    #[test]
    fn transaction_stages_interrupt() {
        let (tasks, source) = tasks();
        let interrupt = Arc::new(Interrupt::default());
        let runnable = Arc::new(Runnables {
            tasks: Some(Arc::clone(&tasks)),
            interrupt: Some(Arc::clone(&interrupt)),
            ..Runnables::default()
        });
        let runtime = Runtime::new(
            Arc::clone(&tasks),
            runnable.clone(),
            Arc::new(Contexts::default()),
            Memory::default(),
        );
        let thread = runtime
            .spawn(
                source,
                &ExecutionCpuSnapshot::X86_64(CpuState::default()),
                linux(REQUIRED),
            )
            .unwrap();
        assert!(runnable.snapshots.lock().unwrap().contains_key(&thread));
        tasks.request_cancellation(thread).unwrap();
        assert!(interrupt.0.load(Ordering::Acquire));
    }

    #[test]
    fn copyout_rollback() {
        let (tasks, source) = tasks();
        let memory = Memory::default();
        memory.bytes.lock().unwrap().insert(0x1000, [0xaa; 4]);
        memory.bytes.lock().unwrap().insert(0x2000, [0xbb; 4]);
        *memory.fail.lock().unwrap() = Some(0x2000);
        let runnable = Arc::new(Runnables::default());
        let contexts = Arc::new(Contexts::default());
        let runtime = Runtime::new(Arc::clone(&tasks), runnable.clone(), contexts.clone(), memory.clone());
        assert_eq!(
            runtime.spawn(
                source,
                &ExecutionCpuSnapshot::X86_64(CpuState::default()),
                linux(REQUIRED | PARENT_SETTID | CHILD_SETTID),
            ),
            Err(Error::Fault)
        );
        assert_eq!(memory.bytes.lock().unwrap()[&0x1000], [0xaa; 4]);
        assert_eq!(memory.bytes.lock().unwrap()[&0x2000], [0xbb; 4]);
        assert_eq!(tasks.snapshot().threads.len(), 1);
        assert!(contexts.0.lock().unwrap().is_empty());
        assert!(runnable.snapshots.lock().unwrap().is_empty());
    }

    #[test]
    fn runnable_rollback() {
        let (tasks, source) = tasks();
        let runnable = Arc::new(Runnables::default());
        *runnable.failure.lock().unwrap() = Some(RuntimeThreadError::Capacity);
        let contexts = Arc::new(Contexts::default());
        let runtime = Runtime::new(Arc::clone(&tasks), runnable, contexts.clone(), Memory::default());
        assert_eq!(
            runtime.spawn(
                source,
                &ExecutionCpuSnapshot::Aarch64(Aarch64CpuState::default()),
                linux(REQUIRED),
            ),
            Err(Error::Again)
        );
        assert_eq!(tasks.snapshot().threads.len(), 1);
        assert!(contexts.0.lock().unwrap().is_empty());
    }
}
