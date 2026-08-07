use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{AioError, Event};

#[derive(Debug)]
pub(crate) struct Context {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
}

#[derive(Debug)]
struct State {
    closing: bool,
    admitted: usize,
    reserved: usize,
    events: VecDeque<Event>,
}

impl Context {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                closing: false,
                admitted: 0,
                reserved: 0,
                events: VecDeque::with_capacity(capacity),
            }),
            changed: Condvar::new(),
            capacity,
        }
    }

    pub(crate) fn admit(self: &Arc<Self>) -> Result<Admission, AioError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closing {
            return Err(AioError::Closing);
        }
        if state.events.len() + state.reserved == self.capacity {
            return Err(AioError::ResourceLimit);
        }
        state.admitted += 1;
        state.reserved += 1;
        Ok(Admission {
            context: Arc::clone(self),
            completed: false,
        })
    }

    pub(crate) fn stage(
        self: &Arc<Self>,
        minimum: usize,
        maximum: usize,
        timeout: Option<Duration>,
        interrupted: impl Fn() -> bool,
    ) -> Result<EventBatch, AioError> {
        if minimum > maximum {
            return Err(AioError::InvalidArgument);
        }
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.events.len() >= minimum || state.closing || maximum == 0 {
                break;
            }
            if interrupted() {
                return Err(AioError::Interrupted);
            }
            let (next, timed_out) = self.wait_until(state, deadline);
            state = next;
            if timed_out {
                break;
            }
        }
        if interrupted() && state.events.len() < minimum {
            return Err(AioError::Interrupted);
        }
        let count = maximum.min(state.events.len());
        let events = state.events.iter().take(count).copied().collect();
        Ok(EventBatch {
            context: Arc::clone(self),
            events,
        })
    }

    fn wait_until<'a>(
        &self,
        state: std::sync::MutexGuard<'a, State>,
        deadline: Option<Instant>,
    ) -> (std::sync::MutexGuard<'a, State>, bool) {
        let Some(deadline) = deadline else {
            return (
                self.changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                false,
            );
        };
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return (state, true);
        };
        let (next, timed) = self
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (next, timed.timed_out())
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closing = true;
        self.changed.notify_all();
        while state.admitted != 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.events.clear();
    }

    pub(crate) fn wake(&self) {
        let _state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.changed.notify_all();
    }
}

pub struct Admission {
    context: Arc<Context>,
    completed: bool,
}

impl Admission {
    pub fn complete(mut self, event: Event) -> Result<(), AioError> {
        let mut state = self
            .context
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.reserved != 0);
        state.reserved -= 1;
        state.admitted -= 1;
        state.events.push_back(event);
        self.completed = true;
        self.context.changed.notify_all();
        Ok(())
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let mut state = self
            .context
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.reserved -= 1;
        state.admitted -= 1;
        self.context.changed.notify_all();
    }
}

pub struct EventBatch {
    context: Arc<Context>,
    events: Vec<Event>,
}

impl EventBatch {
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn commit(self) {
        let mut state = self
            .context
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for _ in 0..self.events.len() {
            let _ = state.events.pop_front();
        }
        self.context.changed.notify_all();
    }
}
