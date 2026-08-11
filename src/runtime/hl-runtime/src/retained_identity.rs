use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_task::{
    ChildSelector, ChildWaitOptions, ExitStatus, PreparedChildWait, ProcessCredentials, ProcessId, ProcessLimits,
    RegistryConfig, TaskRegistry, ThreadId,
};

use crate::{RuntimeTrapOutcome, RuntimeTrapOutcome::Continue};

#[derive(Clone, Copy, Debug)]
struct PublishedTask {
    process: ProcessId,
    thread: ThreadId,
}

#[derive(Debug)]
struct Projection {
    current: ProcessId,
    processes: BTreeMap<ProcessId, u32>,
    threads: BTreeMap<u32, PublishedTask>,
}

/// Runtime task topology plus its retained-engine guest-number projection.
///
/// The generation-qualified identities and parent/thread relationships live in
/// `TaskRegistry`. The small maps only preserve numbers already published by
/// the retained clone implementation while that implementation is migrated.
pub struct RetainedTaskContext {
    tasks: Arc<TaskRegistry>,
    projection: Mutex<Projection>,
    fork_gate: AtomicBool,
}

struct Gate<'context>(&'context AtomicBool);

impl Drop for Gate<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl RetainedTaskContext {
    pub fn new_init(guest_process: u32) -> Result<Self, hl_task::TaskError> {
        if guest_process == 0 {
            return Err(hl_task::TaskError::InvalidSnapshot);
        }
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default())?);
        let credentials = ProcessCredentials::new(0, 0, &[], RegistryConfig::default().max_groups)?;
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::default())?;
        Ok(Self {
            tasks,
            projection: Mutex::new(Projection {
                current: process,
                processes: BTreeMap::from([(process, guest_process)]),
                threads: BTreeMap::from([(guest_process, PublishedTask { process, thread })]),
            }),
            fork_gate: AtomicBool::new(false),
        })
    }

    pub fn dispatch_aarch64(&self, number: u64, guest_thread: u32) -> (RuntimeTrapOutcome, u64) {
        let _gate = self.acquire();
        let projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(task) = projection
            .threads
            .get(&guest_thread)
            .copied()
            .or_else(|| (guest_thread == 0).then(|| Self::leader(&projection)))
        else {
            return (RuntimeTrapOutcome::Fault, 0);
        };
        let value = match number {
            172 => projection.processes.get(&task.process).copied(),
            173 => self
                .tasks
                .process_observation(task.process)
                .ok()
                .and_then(|snapshot| snapshot.parent)
                .map_or(Some(0), |parent| projection.processes.get(&parent).copied()),
            174..=177 => self
                .tasks
                .process_observation(task.process)
                .ok()
                .map(|snapshot| match number {
                    174 => snapshot.credentials.real_user,
                    175 => snapshot.credentials.effective_user,
                    176 => snapshot.credentials.real_group,
                    177 => snapshot.credentials.effective_group,
                    _ => unreachable!(),
                }),
            178 => Some(if guest_thread == 0 {
                projection.processes.get(&task.process).copied().unwrap_or(0)
            } else {
                guest_thread
            }),
            _ => return (RuntimeTrapOutcome::Fault, 0),
        };
        value.map_or((RuntimeTrapOutcome::Fault, 0), |value| (Continue, u64::from(value)))
    }

    pub fn clone_thread(&self, source_guest_thread: u32, child_guest_thread: u32) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        if child_guest_thread == 0 {
            return RuntimeTrapOutcome::Fault;
        }
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if projection.threads.contains_key(&child_guest_thread) {
            return RuntimeTrapOutcome::Fault;
        }
        let Some(source) = projection
            .threads
            .get(&source_guest_thread)
            .copied()
            .or_else(|| (source_guest_thread == 0).then(|| Self::leader(&projection)))
        else {
            return RuntimeTrapOutcome::Fault;
        };
        let Ok(plan) = self.tasks.begin_clone_thread(source.thread) else {
            return RuntimeTrapOutcome::Fault;
        };
        let Ok(thread) = self.tasks.commit_clone_thread(plan) else {
            return RuntimeTrapOutcome::Fault;
        };
        projection.threads.insert(
            child_guest_thread,
            PublishedTask {
                process: source.process,
                thread,
            },
        );
        Continue
    }

    pub fn publish_credentials(&self, guest_thread: u32, values: [u32; 8]) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        let projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(task) = projection.threads.get(&guest_thread).copied() else {
            return RuntimeTrapOutcome::Fault;
        };
        let Ok(snapshot) = self.tasks.process_observation(task.process) else {
            return RuntimeTrapOutcome::Fault;
        };
        let mut credentials = snapshot.credentials;
        credentials.real_user = values[0];
        credentials.effective_user = values[1];
        credentials.saved_user = values[2];
        credentials.filesystem_user = values[3];
        credentials.real_group = values[4];
        credentials.effective_group = values[5];
        credentials.saved_group = values[6];
        credentials.filesystem_group = values[7];
        if self.tasks.replace_credentials(task.process, credentials).is_err() {
            return RuntimeTrapOutcome::Fault;
        }
        Continue
    }

    pub fn fork_process(
        &self,
        source_guest_thread: u32,
        child_guest_process: u32,
        enter_child: bool,
    ) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        self.fork_process_locked(source_guest_thread, child_guest_process, enter_child)
    }

    /// Excludes every registry operation across the retained host `fork()`.
    /// The child inherits an unlocked Rust registry because the preparation
    /// only returns after all earlier users have left it.
    pub fn prepare_fork_process(&self) -> RuntimeTrapOutcome {
        while self
            .fork_gate
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        Continue
    }

    pub fn complete_fork_process(
        &self,
        source_guest_thread: u32,
        child_guest_process: u32,
        enter_child: bool,
    ) -> RuntimeTrapOutcome {
        if !self.fork_gate.load(Ordering::Acquire) {
            return RuntimeTrapOutcome::Fault;
        }
        let outcome = self.fork_process_locked(source_guest_thread, child_guest_process, enter_child);
        self.fork_gate.store(false, Ordering::Release);
        outcome
    }

    pub fn cancel_fork_process(&self) -> RuntimeTrapOutcome {
        if self.fork_gate.swap(false, Ordering::Release) {
            Continue
        } else {
            RuntimeTrapOutcome::Fault
        }
    }

    fn fork_process_locked(
        &self,
        source_guest_thread: u32,
        child_guest_process: u32,
        enter_child: bool,
    ) -> RuntimeTrapOutcome {
        if child_guest_process == 0 {
            return RuntimeTrapOutcome::Fault;
        }
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(source) = projection
            .threads
            .get(&source_guest_thread)
            .copied()
            .or_else(|| (source_guest_thread == 0).then(|| Self::leader(&projection)))
        else {
            return RuntimeTrapOutcome::Fault;
        };
        let child = projection
            .processes
            .iter()
            .find_map(|(process, number)| (*number == child_guest_process).then_some(*process));
        let (process, thread) = if let Some(process) = child {
            let Ok(snapshot) = self.tasks.process_snapshot(process) else {
                return RuntimeTrapOutcome::Fault;
            };
            (process, snapshot.leader)
        } else {
            let Ok(plan) = self.tasks.begin_fork_process(source.thread) else {
                return RuntimeTrapOutcome::Fault;
            };
            let Ok(task) = self.tasks.commit_fork_process(plan) else {
                return RuntimeTrapOutcome::Fault;
            };
            projection.processes.insert(task.0, child_guest_process);
            task
        };
        if enter_child {
            projection.current = process;
            projection.threads.clear();
            projection
                .threads
                .insert(child_guest_process, PublishedTask { process, thread });
        }
        Continue
    }

    pub fn exit_thread(&self, guest_thread: u32) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(task) = projection.threads.get(&guest_thread).copied() else {
            return RuntimeTrapOutcome::Fault;
        };
        if self.tasks.exit_thread(task.thread, ExitStatus::Code(0)).is_err() {
            return RuntimeTrapOutcome::Fault;
        }
        projection.threads.remove(&guest_thread);
        Continue
    }

    /// Retires the Rust task only after the retained wait path has reaped the
    /// corresponding host child. This keeps the registry's bounded process
    /// slots aligned with the C-owned host lifecycle during migration.
    pub fn reap_process(
        &self,
        source_guest_thread: u32,
        child_guest_process: u32,
        wait_status: u32,
    ) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(source) = projection.threads.get(&source_guest_thread).copied() else {
            return RuntimeTrapOutcome::Fault;
        };
        let Some(child) = projection
            .processes
            .iter()
            .find_map(|(process, number)| (*number == child_guest_process).then_some(*process))
        else {
            return RuntimeTrapOutcome::Fault;
        };
        let status = if wait_status & 0x7f == 0 {
            ExitStatus::Code(((wait_status >> 8) & 0xff) as u8)
        } else {
            ExitStatus::Signal {
                signal: (wait_status & 0x7f) as u8,
                dumped_core: wait_status & 0x80 != 0,
            }
        };
        if self.tasks.exit_process(child, status).is_err() {
            return RuntimeTrapOutcome::Fault;
        }
        let Ok(prepared) = self.tasks.prepare_wait_child(
            source.process,
            ChildSelector::Process(child),
            ChildWaitOptions::default(),
        ) else {
            return RuntimeTrapOutcome::Fault;
        };
        let PreparedChildWait::Selection(selection) = prepared else {
            return RuntimeTrapOutcome::Fault;
        };
        if selection.commit().is_err() {
            return RuntimeTrapOutcome::Fault;
        }
        projection.processes.remove(&child);
        projection.threads.retain(|_, task| task.process != child);
        Continue
    }

    pub fn exec_thread(&self, guest_thread: u32) -> RuntimeTrapOutcome {
        let _gate = self.acquire();
        let mut projection = self
            .projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(task) = projection.threads.get(&guest_thread).copied() else {
            return RuntimeTrapOutcome::Fault;
        };
        let Some(process_number) = projection.processes.get(&task.process).copied() else {
            return RuntimeTrapOutcome::Fault;
        };
        let Ok(mut prepared) = self.tasks.prepare_exec(task.process, task.thread) else {
            return RuntimeTrapOutcome::Fault;
        };
        let resulting_thread = prepared.resulting_thread();
        if prepared.publish().is_err() {
            return RuntimeTrapOutcome::Fault;
        }
        prepared.finish();
        projection.threads.clear();
        projection.threads.insert(
            process_number,
            PublishedTask {
                process: task.process,
                thread: resulting_thread,
            },
        );
        Continue
    }

    fn leader(projection: &Projection) -> PublishedTask {
        projection
            .threads
            .values()
            .find(|task| task.process == projection.current)
            .copied()
            .expect("published current process has a leader")
    }

    fn acquire(&self) -> Gate<'_> {
        while self
            .fork_gate
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        Gate(&self.fork_gate)
    }
}

#[cfg(test)]
mod tests {
    use super::RetainedTaskContext;
    use crate::RuntimeTrapOutcome::{Continue, Fault};

    #[test]
    fn fork_parentage_comes_from_the_task_registry() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        assert_eq!(tasks.fork_process(41, 73, false), Continue);
        assert_eq!(tasks.fork_process(41, 73, true), Continue);
        assert_eq!(tasks.dispatch_aarch64(172, 73), (Continue, 73));
        assert_eq!(tasks.dispatch_aarch64(173, 73), (Continue, 41));
    }

    #[test]
    fn fork_gate_can_cross_the_host_fork_boundary() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        assert_eq!(tasks.prepare_fork_process(), Continue);
        assert_eq!(tasks.complete_fork_process(41, 73, true), Continue);
        assert_eq!(tasks.dispatch_aarch64(173, 73), (Continue, 41));
    }

    #[test]
    fn cloned_thread_has_distinct_registry_backed_tid() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        assert_eq!(tasks.clone_thread(41, 1001), Continue);
        assert_eq!(tasks.dispatch_aarch64(172, 1001), (Continue, 41));
        assert_eq!(tasks.dispatch_aarch64(178, 1001), (Continue, 1001));
        assert_eq!(tasks.exit_thread(1001), Continue);
        assert_eq!(tasks.dispatch_aarch64(178, 1001), (Fault, 0));
    }

    #[test]
    fn published_credentials_drive_all_scalar_identity_queries() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        assert_eq!(
            tasks.publish_credentials(41, [10, 11, 12, 13, 20, 21, 22, 23]),
            Continue
        );
        assert_eq!(tasks.dispatch_aarch64(174, 41), (Continue, 10));
        assert_eq!(tasks.dispatch_aarch64(175, 41), (Continue, 11));
        assert_eq!(tasks.dispatch_aarch64(176, 41), (Continue, 20));
        assert_eq!(tasks.dispatch_aarch64(177, 41), (Continue, 21));
    }

    #[test]
    fn nonleader_exec_rebinds_identity_to_the_process_leader() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        assert_eq!(tasks.clone_thread(41, 1001), Continue);
        assert_eq!(tasks.exec_thread(1001), Continue);
        assert_eq!(tasks.dispatch_aarch64(178, 41), (Continue, 41));
        assert_eq!(tasks.dispatch_aarch64(178, 1001), (Fault, 0));
    }

    #[test]
    fn reaped_children_release_bounded_registry_slots() {
        let tasks = RetainedTaskContext::new_init(41).unwrap();
        for child in 1_000..2_100 {
            assert_eq!(tasks.fork_process(41, child, false), Continue);
            assert_eq!(tasks.reap_process(41, child, 7 << 8), Continue);
        }
    }
}
