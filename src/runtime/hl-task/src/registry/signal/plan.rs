use super::TaskRegistry;
use crate::{
    AlternateStack, SignalDisposition, SignalExecPlan, SignalForkPlan, SignalNumber, SignalProcessSnapshot,
    SignalThreadSnapshot, TaskError, ThreadId,
};

impl TaskRegistry {
    pub fn fork_plan(&self, source: ThreadId) -> Result<SignalForkPlan, TaskError> {
        let state = self.lock();
        let thread = Self::thread(&state, source)?;
        let process = Self::process(&state, thread.process)?;
        Ok(SignalForkPlan {
            process: SignalProcessSnapshot {
                actions: Self::nondefault_actions(&process.signals.actions),
                pending: Vec::new(),
            },
            thread: SignalThreadSnapshot {
                mask: thread.signals.mask,
                alternate_stack: thread.signals.alternate_stack,
                pending: Vec::new(),
                deferred: thread.signals.deferred,
                frames: thread.signals.frames.clone(),
            },
        })
    }

    pub fn exec_plan(&self, thread: ThreadId) -> Result<SignalExecPlan, TaskError> {
        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let process = Self::process(&state, thread_state.process)?;
        let actions = process
            .signals
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                (action.disposition == SignalDisposition::Ignore)
                    .then_some((SignalNumber::new((index + 1) as u8).ok()?, *action))
            })
            .collect();
        Ok(SignalExecPlan {
            process: SignalProcessSnapshot {
                actions,
                pending: process.signals.pending.snapshot(),
            },
            thread: SignalThreadSnapshot {
                mask: thread_state.signals.mask,
                alternate_stack: AlternateStack::Disabled,
                pending: thread_state.signals.pending.snapshot(),
                deferred: crate::SignalMask::from_bits(0),
                frames: Vec::new(),
            },
        })
    }
}
