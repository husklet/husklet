use super::TaskRegistry;
use crate::{CpuAffinity, TaskError, ThreadId};

impl TaskRegistry {
    /// Returns a thread's explicit mask, or the instance online set when it has
    /// never been constrained.
    pub fn affinity(&self, thread: ThreadId) -> Result<CpuAffinity, TaskError> {
        let state = self.lock();
        Ok(Self::thread(&state, thread)?
            .affinity
            .unwrap_or_else(|| self.topology.affinity()))
    }

    /// Replaces one live thread's mask after Linux ABI intersection.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuAffinity) -> Result<(), TaskError> {
        if !affinity.is_subset(self.topology.affinity()) {
            return Err(TaskError::InvalidPlan);
        }
        let mut state = self.lock();
        Self::thread_mut(&mut state, thread)?.affinity = Some(affinity);
        Ok(())
    }

    /// Resolves zero as the caller, a TID as that thread, and a PID as its
    /// thread-group leader without consulting unrelated host processes.
    pub fn affinity_target(&self, caller: ThreadId, number: i32) -> Result<ThreadId, TaskError> {
        let state = self.lock();
        if number == 0 {
            Self::thread(&state, caller)?;
            return Ok(caller);
        }
        let number = u32::try_from(number).map_err(|_| TaskError::InvalidThread)?;
        let slot = usize::try_from(number - 1).map_err(|_| TaskError::InvalidThread)?;
        if let Some(entry) = state.threads.get(slot).filter(|entry| entry.value.is_some()) {
            return Ok(ThreadId::new(slot as u32, entry.generation));
        }
        state
            .processes
            .get(slot)
            .and_then(|entry| entry.value.as_ref().map(|process| process.leader))
            .ok_or(TaskError::InvalidThread)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{ExitStatus, Limit, ProcessCredentials, ProcessLimits, RegistryConfig, Resource};

    fn registry(processes: usize, threads: usize) -> (TaskRegistry, ThreadId) {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: processes,
            max_threads: threads,
            max_groups: processes,
            max_pending_signals: 8,
            online_cpus: 1,
        })
        .unwrap();
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let mut limits = ProcessLimits::empty();
        limits.set(Resource::Processes, Limit::new(threads as u64, threads as u64).unwrap());
        let (_, caller) = registry.create_init(credentials, limits).unwrap();
        (registry, caller)
    }

    #[test]
    fn zero_rejects_a_stale_caller_generation() {
        let (registry, leader) = registry(2, 2);
        let stale = registry.begin_clone_thread(leader).unwrap();
        let stale_id = stale.thread();
        registry.rollback_clone_thread(stale).unwrap();

        assert_eq!(registry.affinity_target(stale_id, 0), Err(TaskError::InvalidThread));
    }

    #[test]
    fn reused_thread_number_resolves_the_current_generation() {
        let (registry, leader) = registry(2, 2);
        let parent = registry.snapshot().init.unwrap();
        let (child, exited) = registry
            .commit_fork_process(registry.begin_fork_process(leader).unwrap())
            .unwrap();
        registry.exit_process(child, ExitStatus::Code(0)).unwrap();
        registry.reap(parent, child).unwrap();
        let replacement = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();

        assert_eq!(replacement.number(), exited.number());
        assert_ne!(replacement, exited);
        assert_eq!(
            registry.affinity_target(leader, replacement.number() as i32),
            Ok(replacement)
        );
    }

    #[test]
    fn leader_pid_and_tid_resolve_the_same_leader() {
        let (registry, leader) = registry(2, 2);
        let (child, child_leader) = registry
            .commit_fork_process(registry.begin_fork_process(leader).unwrap())
            .unwrap();

        assert_eq!(child.number(), child_leader.number());
        assert_eq!(
            registry.affinity_target(leader, child.number() as i32),
            Ok(child_leader)
        );
    }

    #[test]
    fn nonleader_tid_cannot_collide_with_an_unrelated_process() {
        let (registry, leader) = registry(3, 3);
        let nonleader = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();
        let fork = registry.begin_fork_process(leader).unwrap();

        assert_ne!(nonleader.number(), fork.process().number());
        registry.commit_fork_process(fork).unwrap();
        assert_eq!(
            registry.affinity_target(leader, nonleader.number() as i32),
            Ok(nonleader)
        );
    }

    #[test]
    fn fork_child_pid_resolves_the_child_leader() {
        let (registry, leader) = registry(3, 4);
        let fork = registry.begin_fork_process(leader).unwrap();
        let child = fork.process();
        let child_leader = fork.thread();
        registry.commit_fork_process(fork).unwrap();

        assert_eq!(
            registry.affinity_target(leader, child.number() as i32),
            Ok(child_leader)
        );
    }

    #[test]
    fn nonleader_exec_retires_caller_number_until_reuse() {
        let (registry, leader) = registry(2, 2);
        let registry = Arc::new(registry);
        let process = registry.snapshot().init.unwrap();
        let caller = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();
        let mut exec = registry.prepare_exec(process, caller).unwrap();
        exec.publish().unwrap();

        assert_eq!(
            registry.affinity_target(leader, caller.number() as i32),
            Err(TaskError::InvalidThread)
        );
        assert_eq!(registry.affinity_target(leader, leader.number() as i32), Ok(leader));
        exec.finish();

        let replacement = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();
        assert_eq!(replacement.number(), caller.number());
        assert_ne!(replacement, caller);
        assert_eq!(
            registry.affinity_target(leader, replacement.number() as i32),
            Ok(replacement)
        );
    }

    #[test]
    fn numeric_boundaries_are_checked() {
        let (registry, caller) = registry(2, 2);

        assert_eq!(registry.affinity_target(caller, 0), Ok(caller));
        for number in [-1, 3, i32::MAX] {
            assert_eq!(registry.affinity_target(caller, number), Err(TaskError::InvalidThread));
        }
    }
}
