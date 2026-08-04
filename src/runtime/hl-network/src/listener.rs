use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use crate::{SocketAddress, SocketHostIo};

pub struct AcceptedToken<T> {
    pub token: T,
    pub local: SocketAddress,
    pub peer: SocketAddress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptError {
    WouldBlock,
    Canceled,
    Backpressure,
}

struct ListenerState<T> {
    backlog: usize,
    canceled: bool,
    queue: VecDeque<AcceptedToken<T>>,
    next_ticket: u64,
    serving_ticket: u64,
}

impl<T> ListenerState<T> {
    fn take_for(&mut self, ticket: u64) -> Option<AcceptedToken<T>> {
        if ticket != self.serving_ticket {
            return None;
        }
        let accepted = self.queue.pop_front()?;
        self.serving_ticket = self.serving_ticket.wrapping_add(1);
        Some(accepted)
    }
}

pub struct ListenerQueue<H: SocketHostIo> {
    host: Arc<H>,
    state: Mutex<ListenerState<H::Token>>,
    wake: Condvar,
}

impl<H: SocketHostIo> ListenerQueue<H> {
    pub fn new(host: Arc<H>) -> Self {
        Self {
            host,
            state: Mutex::new(ListenerState {
                backlog: 1,
                canceled: false,
                queue: VecDeque::new(),
                next_ticket: 0,
                serving_ticket: 0,
            }),
            wake: Condvar::new(),
        }
    }

    pub fn set_backlog(&self, backlog: usize) {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).backlog = backlog.max(1);
    }

    pub fn publish(&self, accepted: AcceptedToken<H::Token>) -> Result<(), AcceptError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let error = if state.canceled {
            Some(AcceptError::Canceled)
        } else if state.queue.len() >= state.backlog {
            Some(AcceptError::Backpressure)
        } else {
            None
        };
        if let Some(error) = error {
            drop(state);
            self.host.close(accepted.token);
            return Err(error);
        }
        state.queue.push_back(accepted);
        drop(state);
        self.wake.notify_all();
        Ok(())
    }

    pub fn accept(&self, nonblocking: bool) -> Result<AcceptedToken<H::Token>, AcceptError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if nonblocking {
            return state.queue.pop_front().ok_or(if state.canceled {
                AcceptError::Canceled
            } else {
                AcceptError::WouldBlock
            });
        }
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        loop {
            if let Some(accepted) = state.take_for(ticket) {
                drop(state);
                self.wake.notify_all();
                return Ok(accepted);
            }
            if state.canceled {
                return Err(AcceptError::Canceled);
            }
            state = self.wake.wait(state).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub fn readable(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .queue
            .is_empty()
    }

    pub fn cancel_and_drain(&self) {
        let tokens = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.canceled {
                return;
            }
            state.canceled = true;
            state.queue.drain(..).map(|accepted| accepted.token).collect::<Vec<_>>()
        };
        tokens.into_iter().for_each(|token| self.host.close(token));
        self.wake.notify_all();
    }

    #[cfg(test)]
    pub fn notify_spurious(&self) {
        self.wake.notify_all();
    }

    #[cfg(test)]
    pub fn waiting(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.next_ticket - state.serving_ticket
    }
}
