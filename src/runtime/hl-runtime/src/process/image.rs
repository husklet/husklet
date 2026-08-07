use std::sync::{Arc, Mutex};

use hl_linux::ExecPlan;
use hl_task::{ProcessId, ThreadId};

use crate::{PreparedExec, RuntimeExecError, RuntimeExecPort};

const MAXIMUM_EXEC_PARTICIPANTS: usize = 16;

/// One prepared part of an out-of-place process-image replacement.
///
/// `rollback` must restore the exact pre-publish state whether or not
/// `publish` succeeded. `finish` releases retained old state only after every
/// participant has published successfully.
pub trait PreparedExecParticipant: Send {
    fn publish(&mut self) -> Result<(), RuntimeExecError>;
    fn rollback(&mut self);
    fn finish(&mut self);
}

/// Consumer-owned capability that stages one exec domain without publication.
pub trait RuntimeExecParticipant: Send + Sync {
    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError>;
}

pub struct RejectingExecPort;

impl RuntimeExecPort for RejectingExecPort {
    fn prepare(&self, _: ProcessId, _: ThreadId, _: ExecPlan) -> Result<Box<dyn PreparedExec>, RuntimeExecError> {
        Err(RuntimeExecError::Unsupported)
    }
}

/// Safe exec backend that coordinates only reversible, out-of-place stages.
pub struct SafeRuntimeExec {
    participants: Vec<Arc<dyn RuntimeExecParticipant>>,
    publish_order: Vec<usize>,
    transaction: Arc<Mutex<bool>>,
}

/// An exec transaction staged completely outside the running process image.
///
/// Dropping this value before `commit` rolls every participant back. This is
/// the scheduler boundary: publication may be deferred until the old CPU lock
/// has been released without leaking staged resources on cancellation.
pub struct PreparedRuntimeExec {
    prepared: Vec<Box<dyn PreparedExecParticipant>>,
    publish_order: Vec<usize>,
    transaction: Arc<Mutex<bool>>,
    complete: bool,
}

impl SafeRuntimeExec {
    pub fn new(participants: Vec<Arc<dyn RuntimeExecParticipant>>) -> Result<Self, RuntimeExecError> {
        if participants.is_empty() || participants.len() > MAXIMUM_EXEC_PARTICIPANTS {
            return Err(RuntimeExecError::Unsupported);
        }
        let publish_order = (0..participants.len()).collect();
        Self::with_publish_order(participants, publish_order)
    }

    pub fn with_publish_order(
        participants: Vec<Arc<dyn RuntimeExecParticipant>>,
        publish_order: Vec<usize>,
    ) -> Result<Self, RuntimeExecError> {
        if participants.is_empty()
            || participants.len() > MAXIMUM_EXEC_PARTICIPANTS
            || publish_order.len() != participants.len()
        {
            return Err(RuntimeExecError::Unsupported);
        }
        let mut sorted = publish_order.clone();
        sorted.sort_unstable();
        if sorted != (0..participants.len()).collect::<Vec<_>>() {
            return Err(RuntimeExecError::Invalid);
        }
        Ok(Self {
            participants,
            publish_order,
            transaction: Arc::new(Mutex::new(false)),
        })
    }

    pub fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &ExecPlan,
    ) -> Result<PreparedRuntimeExec, RuntimeExecError> {
        {
            let mut active = self
                .transaction
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active {
                return Err(RuntimeExecError::Failed);
            }
            *active = true;
        }
        match self.stage(process, thread, plan) {
            Ok(prepared) => Ok(PreparedRuntimeExec {
                prepared,
                publish_order: self.publish_order.clone(),
                transaction: Arc::clone(&self.transaction),
                complete: false,
            }),
            Err(error) => {
                *self
                    .transaction
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                Err(error)
            }
        }
    }

    fn stage(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: &ExecPlan,
    ) -> Result<Vec<Box<dyn PreparedExecParticipant>>, RuntimeExecError> {
        let mut prepared = Vec::with_capacity(self.participants.len());
        for participant in &self.participants {
            match participant.prepare(process, thread, plan) {
                Ok(value) => prepared.push(value),
                Err(error) => {
                    Self::rollback_prepared(&mut prepared);
                    return Err(error);
                }
            }
        }
        Ok(prepared)
    }

    fn rollback_prepared(prepared: &mut [Box<dyn PreparedExecParticipant>]) {
        for participant in prepared.iter_mut().rev() {
            participant.rollback();
        }
    }
}

impl PreparedRuntimeExec {
    pub fn commit(mut self) -> Result<(), RuntimeExecError> {
        for index in &self.publish_order {
            self.prepared[*index].publish()?;
        }
        for participant in &mut self.prepared {
            participant.finish();
        }
        self.complete = true;
        self.release();
        Ok(())
    }

    fn rollback(&mut self) {
        for index in self.publish_order.iter().rev() {
            self.prepared[*index].rollback();
        }
    }

    fn release(&self) {
        *self
            .transaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }
}

impl PreparedExec for PreparedRuntimeExec {
    fn commit(self: Box<Self>) -> Result<(), RuntimeExecError> {
        Self::commit(*self)
    }
}

impl Drop for PreparedRuntimeExec {
    fn drop(&mut self) {
        if !self.complete {
            self.rollback();
            self.release();
        }
    }
}

impl RuntimeExecPort for SafeRuntimeExec {
    fn prepare(
        &self,
        process: ProcessId,
        thread: ThreadId,
        plan: ExecPlan,
    ) -> Result<Box<dyn PreparedExec>, RuntimeExecError> {
        Ok(Box::new(self.prepare(process, thread, &plan)?))
    }
}

struct ImageState<I> {
    generation: u64,
    current: Arc<I>,
}

/// Atomic owner of the complete execution image selected by one process.
pub struct Image<I> {
    state: Arc<Mutex<ImageState<I>>>,
}

impl<I> Image<I> {
    #[must_use]
    pub fn new(image: I) -> Self {
        Self {
            state: Arc::new(Mutex::new(ImageState {
                generation: 1,
                current: Arc::new(image),
            })),
        }
    }

    #[must_use]
    pub fn current(&self) -> (u64, Arc<I>) {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.generation, Arc::clone(&state.current))
    }

    #[must_use]
    pub fn prepare(&self, expected: u64, replacement: I) -> PreparedProcessImage<I> {
        PreparedProcessImage {
            state: Arc::clone(&self.state),
            expected,
            replacement: Some(Arc::new(replacement)),
            previous: None,
        }
    }
}

pub struct PreparedProcessImage<I> {
    state: Arc<Mutex<ImageState<I>>>,
    expected: u64,
    replacement: Option<Arc<I>>,
    previous: Option<Arc<I>>,
}

impl<I> PreparedProcessImage<I> {
    #[must_use]
    pub fn candidate(&self) -> Option<Arc<I>> {
        self.replacement.clone()
    }
}

impl<I: Send + Sync + 'static> PreparedExecParticipant for PreparedProcessImage<I> {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation != self.expected || self.previous.is_some() {
            return Err(RuntimeExecError::Failed);
        }
        let replacement = self.replacement.take().ok_or(RuntimeExecError::Failed)?;
        self.previous = Some(std::mem::replace(&mut state.current, replacement));
        state.generation = state.generation.wrapping_add(1).max(1);
        Ok(())
    }

    fn rollback(&mut self) {
        let Some(previous) = self.previous.take() else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.current = previous;
        state.generation = self.expected;
    }

    fn finish(&mut self) {
        self.previous = None;
    }
}
