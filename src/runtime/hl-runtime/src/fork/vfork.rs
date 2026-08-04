use std::sync::{Arc, Mutex};

use hl_task::{ProcessId, ThreadId};

pub trait VforkWake: Send + Sync {
    fn resume(&self, parent: ThreadId) -> Result<(), ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VforkError {
    WrongChild,
    Released,
    Wake,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Active,
    Released,
}

pub struct VforkParentToken {
    parent: ThreadId,
    child: ProcessId,
    wake: Arc<dyn VforkWake>,
    state: Mutex<State>,
}

impl VforkParentToken {
    #[must_use]
    pub fn new(parent: ThreadId, child: ProcessId, wake: Arc<dyn VforkWake>) -> Self {
        Self {
            parent,
            child,
            wake,
            state: Mutex::new(State::Active),
        }
    }

    pub fn release(&self, child: ProcessId) -> Result<(), VforkError> {
        if child != self.child {
            return Err(VforkError::WrongChild);
        }
        let mut state = self.state.lock().map_err(|_| VforkError::Wake)?;
        if *state == State::Released {
            return Err(VforkError::Released);
        }
        self.wake.resume(self.parent).map_err(|()| VforkError::Wake)?;
        *state = State::Released;
        Ok(())
    }

    pub fn rollback(&self) -> Result<(), VforkError> {
        self.release(self.child)
    }

    #[must_use]
    pub fn released(&self) -> bool {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) == State::Released
    }
}

impl Drop for VforkParentToken {
    fn drop(&mut self) {
        if *self.state.get_mut().unwrap_or_else(|error| error.into_inner()) == State::Active
            && self.wake.resume(self.parent).is_ok()
        {
            *self.state.get_mut().unwrap_or_else(|error| error.into_inner()) = State::Released;
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Wake(AtomicUsize);

    impl VforkWake for Wake {
        fn resume(&self, _: ThreadId) -> Result<(), ()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn identities() -> (ThreadId, ProcessId) {
        (ThreadId::from_wire(1, 1).unwrap(), ProcessId::from_wire(2, 1).unwrap())
    }

    #[test]
    fn release_is_exactly_once() {
        let (parent, child) = identities();
        let wake = Arc::new(Wake(AtomicUsize::new(0)));
        let token = VforkParentToken::new(parent, child, wake.clone());
        assert_eq!(token.release(child), Ok(()));
        assert_eq!(token.release(child), Err(VforkError::Released));
        assert_eq!(wake.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_rolls_back_parent() {
        let (parent, child) = identities();
        let wake = Arc::new(Wake(AtomicUsize::new(0)));
        drop(VforkParentToken::new(parent, child, wake.clone()));
        assert_eq!(wake.0.load(Ordering::Relaxed), 1);
    }
}
