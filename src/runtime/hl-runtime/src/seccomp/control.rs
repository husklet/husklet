use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use hl_linux::{FilterInstallPlan, SeccompData, SeccompDecision, SeccompPolicy, SeccompPolicyError, SyscallFrame};
use hl_task::ThreadId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerRequest {
    pub owner: ThreadId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicySnapshot {
    pub policies: Vec<(ThreadId, SeccompPolicy)>,
}

pub struct RestoreTransaction {
    version: u64,
    policies: Option<BTreeMap<ThreadId, SeccompPolicy>>,
    previous: Option<BTreeMap<ThreadId, SeccompPolicy>>,
    committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    DuplicateThread,
    MissingThread,
    InvalidTargets,
    PolicyDivergence(ThreadId),
    Capacity,
    Conflict,
    Policy(SeccompPolicyError),
}

impl From<SeccompPolicyError> for ControlError {
    fn from(value: SeccompPolicyError) -> Self {
        Self::Policy(value)
    }
}

#[derive(Debug)]
#[must_use = "a seccomp install transaction must be committed or rolled back"]
pub struct InstallTransaction {
    version: u64,
    policies: BTreeMap<ThreadId, SeccompPolicy>,
    listener: Option<ListenerRequest>,
}

impl InstallTransaction {
    #[must_use]
    pub const fn listener(&self) -> Option<ListenerRequest> {
        self.listener
    }
}

struct State {
    version: u64,
    policies: BTreeMap<ThreadId, SeccompPolicy>,
    checkpoint_frozen: bool,
}

pub struct Control {
    maximum_threads: usize,
    ever_active: AtomicBool,
    state: Mutex<State>,
}

impl Control {
    pub fn new(maximum_threads: usize) -> Result<Self, ControlError> {
        if maximum_threads == 0 {
            return Err(ControlError::Capacity);
        }
        Ok(Self {
            maximum_threads,
            ever_active: AtomicBool::new(false),
            state: Mutex::new(State {
                version: 1,
                policies: BTreeMap::new(),
                checkpoint_frozen: false,
            }),
        })
    }

    /// Reports whether any policy installation has made the admission path
    /// observable. The flag is monotonic so a false result permits an
    /// ordinary syscall to bypass policy storage without racing installation.
    #[must_use]
    pub fn requires_evaluation(&self) -> bool {
        self.ever_active.load(Ordering::Acquire)
    }

    pub fn register(&self, thread: ThreadId) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        if state.policies.contains_key(&thread) {
            return Err(ControlError::DuplicateThread);
        }
        if state.policies.len() >= self.maximum_threads {
            return Err(ControlError::Capacity);
        }
        state.policies.insert(thread, SeccompPolicy::default());
        Self::advance(&mut state);
        Ok(())
    }

    pub fn register_inheriting(&self, thread: ThreadId, siblings: &[ThreadId]) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        if state.policies.contains_key(&thread) {
            return Ok(());
        }
        if state.policies.len() >= self.maximum_threads {
            return Err(ControlError::Capacity);
        }
        let policy = siblings
            .iter()
            .find_map(|sibling| state.policies.get(sibling))
            .cloned()
            .unwrap_or_default();
        state.policies.insert(thread, policy);
        Self::advance(&mut state);
        Ok(())
    }

    pub fn mode(&self, thread: ThreadId) -> Result<hl_linux::SeccompMode, ControlError> {
        self.lock()
            .policies
            .get(&thread)
            .map(hl_linux::SeccompPolicy::mode)
            .ok_or(ControlError::MissingThread)
    }

    pub fn status(
        &self,
        thread: ThreadId,
        baseline: hl_linux::SeccompBaseline,
    ) -> Result<hl_linux::SeccompStatus, ControlError> {
        let state = self.lock();
        let policy = state.policies.get(&thread).ok_or(ControlError::MissingThread)?;
        Ok(match policy.mode() {
            hl_linux::SeccompMode::Disabled => baseline.status(),
            hl_linux::SeccompMode::Strict => hl_linux::SeccompStatus {
                mode: hl_linux::SeccompMode::Strict,
                filters: 0,
            },
            hl_linux::SeccompMode::Filter => hl_linux::SeccompStatus {
                mode: hl_linux::SeccompMode::Filter,
                filters: baseline.status().filters + policy.filter_count(),
            },
        })
    }

    pub fn unregister(&self, thread: ThreadId) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        state.policies.remove(&thread).ok_or(ControlError::MissingThread)?;
        Self::advance(&mut state);
        Ok(())
    }

    pub fn lock_privileges(&self, thread: ThreadId) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        state
            .policies
            .get_mut(&thread)
            .ok_or(ControlError::MissingThread)?
            .enable_nnp();
        Self::advance(&mut state);
        Ok(())
    }

    pub fn enable_strict(&self, thread: ThreadId) -> Result<(), ControlError> {
        // Publish before taking the policy lock. A concurrent evaluator must
        // either observe the old disabled state before this operation starts,
        // or enter the locked path and serialize with the installation.
        self.ever_active.store(true, Ordering::Release);
        let mut state = self.lock();
        Self::mutable(&state)?;
        state
            .policies
            .get_mut(&thread)
            .ok_or(ControlError::MissingThread)?
            .strict()?;
        Self::advance(&mut state);
        Ok(())
    }

    pub fn begin_install(
        &self,
        caller: ThreadId,
        process_threads: &[ThreadId],
        plan: FilterInstallPlan,
        has_admin_capability: bool,
    ) -> Result<InstallTransaction, ControlError> {
        let state = self.lock();
        Self::mutable(&state)?;
        let targets = Self::targets(caller, process_threads, &plan)?;
        let mut policies = state.policies.clone();
        let caller_policy = policies.get(&caller).ok_or(ControlError::MissingThread)?.clone();
        if !caller_policy.no_new_privileges() && !has_admin_capability {
            return Err(ControlError::Policy(SeccompPolicyError::PermissionDenied));
        }
        if plan.flags.synchronize_threads {
            Self::validate_synchronization(&policies, &targets, &caller_policy)?;
        }
        for target in targets {
            policies
                .get_mut(&target)
                .ok_or(ControlError::MissingThread)?
                .install(plan.clone(), true, true)?;
        }
        let listener = plan.listener_requested.then_some(ListenerRequest { owner: caller });
        Ok(InstallTransaction {
            version: state.version,
            policies,
            listener,
        })
    }

    pub fn commit_install(&self, transaction: InstallTransaction) -> Result<Option<ListenerRequest>, ControlError> {
        // This bit is deliberately monotonic. Failed or later-retired filters
        // only make evaluation conservative; they can never bypass policy.
        self.ever_active.store(true, Ordering::Release);
        let mut state = self.lock();
        Self::mutable(&state)?;
        if state.version != transaction.version {
            return Err(ControlError::Conflict);
        }
        state.policies = transaction.policies;
        Self::advance(&mut state);
        Ok(transaction.listener)
    }

    pub fn rollback_install(&self, _transaction: InstallTransaction) {}

    pub fn evaluate(&self, thread: ThreadId, data: SeccompData) -> Result<SeccompDecision, ControlError> {
        let state = self.lock();
        Ok(state
            .policies
            .get(&thread)
            .ok_or(ControlError::MissingThread)?
            .decide(data))
    }

    pub fn evaluate_syscall(
        &self,
        thread: ThreadId,
        frame: &SyscallFrame,
        instruction_pointer: u64,
    ) -> Result<SeccompDecision, ControlError> {
        self.evaluate(
            thread,
            SeccompData {
                number: frame.raw_number as i32,
                architecture: SeccompData::audit_arch(frame.architecture),
                instruction_pointer,
                arguments: frame.arguments,
            },
        )
    }

    /// Evaluates a syscall for a thread whose registration is held by the
    /// caller's runtime lifetime.
    ///
    /// Before any seccomp policy has ever been activated, all registered
    /// policies are necessarily disabled, so ordinary syscall admission does
    /// not need the global policy lock or thread-map lookup. Once activation
    /// starts this permanently takes the exact locked path above, including
    /// after rollback or retirement.
    pub fn evaluate_registered_syscall(
        &self,
        thread: ThreadId,
        frame: &SyscallFrame,
        instruction_pointer: u64,
    ) -> Result<SeccompDecision, ControlError> {
        if !self.requires_evaluation() {
            return Ok(SeccompDecision::Continue);
        }
        self.evaluate_syscall(thread, frame, instruction_pointer)
    }

    pub fn fork(&self, source: ThreadId, child: ThreadId) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        if state.policies.contains_key(&child) {
            return Err(ControlError::DuplicateThread);
        }
        if state.policies.len() >= self.maximum_threads {
            return Err(ControlError::Capacity);
        }
        let policy = state
            .policies
            .get(&source)
            .ok_or(ControlError::MissingThread)?
            .fork_snapshot();
        state.policies.insert(child, policy);
        Self::advance(&mut state);
        Ok(())
    }

    pub fn exec(&self, thread: ThreadId) -> Result<(), ControlError> {
        let mut state = self.lock();
        Self::mutable(&state)?;
        let policy = state
            .policies
            .get(&thread)
            .ok_or(ControlError::MissingThread)?
            .exec_snapshot();
        state.policies.insert(thread, policy);
        Self::advance(&mut state);
        Ok(())
    }

    pub fn snapshot(&self) -> PolicySnapshot {
        let state = self.lock();
        PolicySnapshot {
            policies: state
                .policies
                .iter()
                .map(|(thread, policy)| (*thread, policy.clone()))
                .collect(),
        }
    }

    pub fn freeze_checkpoint(&self) -> Result<(), ControlError> {
        let mut state = self.lock();
        if state.checkpoint_frozen {
            return Err(ControlError::Conflict);
        }
        state.checkpoint_frozen = true;
        Ok(())
    }

    pub fn thaw_checkpoint(&self) {
        self.lock().checkpoint_frozen = false;
    }

    pub fn stage_checkpoint(&self, snapshot: &PolicySnapshot) -> Result<RestoreTransaction, ControlError> {
        if snapshot.policies.len() > self.maximum_threads {
            return Err(ControlError::Capacity);
        }
        let mut policies = BTreeMap::new();
        for (thread, policy) in &snapshot.policies {
            let policy = SeccompPolicy::restore_checkpoint(&policy.checkpoint_image())?;
            if policies.insert(*thread, policy).is_some() {
                return Err(ControlError::DuplicateThread);
            }
        }
        let state = self.lock();
        Ok(RestoreTransaction {
            version: state.version,
            policies: Some(policies),
            previous: None,
            committed: false,
        })
    }

    pub fn commit_checkpoint(&self, transaction: &mut RestoreTransaction) -> Result<(), ControlError> {
        if transaction.committed {
            return Err(ControlError::Conflict);
        }
        let replacement = transaction.policies.take().ok_or(ControlError::Conflict)?;
        // A restored snapshot may contain an active policy. Conservatively
        // disable the fast path even when it does not; correctness does not
        // depend on inspecting policy contents here.
        self.ever_active.store(true, Ordering::Release);
        let mut state = self.lock();
        if state.version != transaction.version {
            transaction.policies = Some(replacement);
            return Err(ControlError::Conflict);
        }
        transaction.previous = Some(std::mem::replace(&mut state.policies, replacement));
        Self::advance(&mut state);
        transaction.committed = true;
        Ok(())
    }

    pub fn rollback_checkpoint(&self, transaction: &mut RestoreTransaction) {
        if transaction.committed
            && let Some(previous) = transaction.previous.take()
        {
            let mut state = self.lock();
            state.policies = previous;
            Self::advance(&mut state);
        }
        transaction.committed = false;
    }

    pub fn resume_checkpoint(&self, transaction: &RestoreTransaction) -> Result<(), ControlError> {
        if transaction.committed {
            Ok(())
        } else {
            Err(ControlError::Conflict)
        }
    }

    fn targets(
        caller: ThreadId,
        process_threads: &[ThreadId],
        plan: &FilterInstallPlan,
    ) -> Result<Vec<ThreadId>, ControlError> {
        if !plan.flags.synchronize_threads {
            return Ok(vec![caller]);
        }
        let targets = process_threads.iter().copied().collect::<BTreeSet<_>>();
        if targets.len() != process_threads.len() || !targets.contains(&caller) {
            return Err(ControlError::InvalidTargets);
        }
        Ok(targets.into_iter().collect())
    }

    fn validate_synchronization(
        policies: &BTreeMap<ThreadId, SeccompPolicy>,
        targets: &[ThreadId],
        caller: &SeccompPolicy,
    ) -> Result<(), ControlError> {
        for target in targets {
            let policy = policies.get(target).ok_or(ControlError::MissingThread)?;
            if !policy.same_filter_chain(caller) {
                return Err(ControlError::PolicyDivergence(*target));
            }
        }
        Ok(())
    }

    fn advance(state: &mut State) {
        state.version = state.version.wrapping_add(1).max(1);
    }

    fn mutable(state: &State) -> Result<(), ControlError> {
        if state.checkpoint_frozen {
            Err(ControlError::Conflict)
        } else {
            Ok(())
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
