use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use hl_descriptor::StatusFlags;

use crate::{SocketType, UnixAddress, UnixSocketPair};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NamedSocketState {
    Created,
    Bound { address: UnixAddress },
    Listening { address: UnixAddress, backlog: usize },
    Connected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamedSocketError {
    InvalidTransition,
    UnsupportedType,
    TypeMismatch,
    WouldBlock,
    Backpressure,
}

struct State {
    lifecycle: NamedSocketState,
    pending: VecDeque<Arc<UnixSocketPair>>,
    reserved: usize,
    connecting: bool,
}

/// An owned claim on one listener backlog slot and one connecting client.
///
/// Dropping an uncommitted reservation releases both claims. This lets callers
/// perform a separate catalog transaction before making the connection visible
/// to either `accept` or the client lifecycle.
pub struct ConnectReservation {
    client: Arc<NamedSocket>,
    listener: Arc<NamedSocket>,
    active: bool,
}

/// Runtime-neutral lifecycle and accept queue for a named Unix connection socket.
///
/// A queued pair's endpoint zero belongs to the connecting socket and endpoint
/// one belongs to the socket returned by `accept`.
pub struct NamedSocket {
    socket_type: SocketType,
    state: Mutex<State>,
    wake: Condvar,
    readiness: Arc<hl_sync::WaitQueue>,
}

impl NamedSocket {
    pub fn new(socket_type: SocketType) -> Result<Self, NamedSocketError> {
        if !matches!(socket_type, SocketType::Stream | SocketType::SequencePacket) {
            return Err(NamedSocketError::UnsupportedType);
        }
        Ok(Self {
            socket_type,
            state: Mutex::new(State {
                lifecycle: NamedSocketState::Created,
                pending: VecDeque::new(),
                reserved: 0,
                connecting: false,
            }),
            wake: Condvar::new(),
            readiness: Arc::new(hl_sync::WaitQueue::new()),
        })
    }

    #[must_use]
    pub const fn socket_type(&self) -> SocketType {
        self.socket_type
    }

    #[must_use]
    pub fn state(&self) -> NamedSocketState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lifecycle
            .clone()
    }

    pub fn bind(&self, address: UnixAddress) -> Result<(), NamedSocketError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.connecting || state.lifecycle != NamedSocketState::Created {
            return Err(NamedSocketError::InvalidTransition);
        }
        state.lifecycle = NamedSocketState::Bound { address };
        Ok(())
    }

    pub fn listen(&self, backlog: usize) -> Result<(), NamedSocketError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.connecting {
            return Err(NamedSocketError::InvalidTransition);
        }
        let address = match &state.lifecycle {
            NamedSocketState::Bound { address } | NamedSocketState::Listening { address, .. } => address.clone(),
            _ => return Err(NamedSocketError::InvalidTransition),
        };
        let backlog = backlog.max(1);
        state.lifecycle = NamedSocketState::Listening { address, backlog };
        drop(state);
        self.wake.notify_all();
        self.readiness.notify_all();
        Ok(())
    }

    #[must_use]
    pub fn wait_queue(&self) -> Arc<hl_sync::WaitQueue> {
        self.readiness.clone()
    }

    pub fn connect(
        self: &Arc<Self>,
        listener: &Arc<Self>,
        flags: StatusFlags,
        nonblocking: bool,
    ) -> Result<Arc<UnixSocketPair>, NamedSocketError> {
        let pair =
            Arc::new(UnixSocketPair::new(self.socket_type, flags).map_err(|_| NamedSocketError::UnsupportedType)?);
        self.reserve_connect(listener, nonblocking)?.commit(pair.clone())?;
        Ok(pair)
    }

    /// Reserves listener capacity without exposing a pending connection.
    pub fn reserve_connect(
        self: &Arc<Self>,
        listener: &Arc<Self>,
        nonblocking: bool,
    ) -> Result<ConnectReservation, NamedSocketError> {
        if Arc::ptr_eq(self, listener) {
            return Err(NamedSocketError::InvalidTransition);
        }
        if self.socket_type != listener.socket_type {
            return Err(NamedSocketError::TypeMismatch);
        }
        {
            let mut client = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if client.connecting
                || !matches!(
                    client.lifecycle,
                    NamedSocketState::Created | NamedSocketState::Bound { .. }
                )
            {
                return Err(NamedSocketError::InvalidTransition);
            }
            client.connecting = true;
        }

        let mut state = listener.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let backlog = if let NamedSocketState::Listening { backlog, .. } = state.lifecycle { backlog } else {
                drop(state);
                self.cancel_connecting();
                return Err(NamedSocketError::InvalidTransition);
            };
            if state.pending.len() + state.reserved < backlog {
                state.reserved += 1;
                return Ok(ConnectReservation {
                    client: self.clone(),
                    listener: listener.clone(),
                    active: true,
                });
            }
            if nonblocking {
                drop(state);
                self.cancel_connecting();
                return Err(NamedSocketError::WouldBlock);
            }
            state = listener.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn cancel_connecting(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.connecting = false;
        drop(state);
        self.wake.notify_all();
    }

    /// Adds a preconstructed connection to the listener as one atomic FIFO operation.
    pub fn enqueue(&self, pair: Arc<UnixSocketPair>, nonblocking: bool) -> Result<(), NamedSocketError> {
        if pair.socket_type() != self.socket_type {
            return Err(NamedSocketError::TypeMismatch);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let backlog = match state.lifecycle {
                NamedSocketState::Listening { backlog, .. } => backlog,
                _ => return Err(NamedSocketError::InvalidTransition),
            };
            if state.pending.len() + state.reserved < backlog {
                state.pending.push_back(pair);
                drop(state);
                self.wake.notify_all();
                self.readiness.notify_all();
                return Ok(());
            }
            if nonblocking {
                return Err(NamedSocketError::WouldBlock);
            }
            state = self.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Removes the oldest pending connection as one atomic FIFO operation.
    pub fn accept(&self, nonblocking: bool) -> Result<Arc<UnixSocketPair>, NamedSocketError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if !matches!(state.lifecycle, NamedSocketState::Listening { .. }) {
                return Err(NamedSocketError::InvalidTransition);
            }
            if let Some(pair) = state.pending.pop_front() {
                drop(state);
                self.wake.notify_all();
                self.readiness.notify_all();
                return Ok(pair);
            }
            if nonblocking {
                return Err(NamedSocketError::WouldBlock);
            }
            state = self.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[must_use]
    pub fn readable(&self) -> bool {
        self.pending() != 0
    }

    #[must_use]
    pub fn pending(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .len()
    }
}

impl ConnectReservation {
    /// Publishes the pair to the listener and changes the client to connected
    /// as one critical section after the caller's external transaction succeeds.
    pub fn commit(mut self, pair: Arc<UnixSocketPair>) -> Result<(), NamedSocketError> {
        if pair.socket_type() != self.client.socket_type {
            return Err(NamedSocketError::TypeMismatch);
        }
        let mut listener = self.listener.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(listener.lifecycle, NamedSocketState::Listening { .. }) || listener.reserved == 0 {
            return Err(NamedSocketError::InvalidTransition);
        }
        let mut client = self.client.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !client.connecting
            || !matches!(
                client.lifecycle,
                NamedSocketState::Created | NamedSocketState::Bound { .. }
            )
        {
            return Err(NamedSocketError::InvalidTransition);
        }
        listener.reserved -= 1;
        listener.pending.push_back(pair);
        client.connecting = false;
        client.lifecycle = NamedSocketState::Connected;
        self.active = false;
        drop(client);
        drop(listener);
        self.listener.wake.notify_all();
        self.client.wake.notify_all();
        self.listener.readiness.notify_all();
        self.client.readiness.notify_all();
        Ok(())
    }
}

impl Drop for ConnectReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut listener = self.listener.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert_ne!(listener.reserved, 0);
        listener.reserved = listener.reserved.saturating_sub(1);
        let mut client = self.client.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        client.connecting = false;
        drop(client);
        drop(listener);
        self.listener.wake.notify_all();
        self.client.wake.notify_all();
        self.listener.readiness.notify_all();
        self.client.readiness.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use hl_descriptor::StatusFlags;

    use super::{NamedSocket, NamedSocketError, NamedSocketState};
    use crate::{SocketType, UnixAddress, UnixSocketPair};

    fn listener(socket_type: SocketType, backlog: usize) -> Arc<NamedSocket> {
        let socket = Arc::new(NamedSocket::new(socket_type).unwrap());
        socket.bind(UnixAddress::Abstract(b"service".to_vec())).unwrap();
        socket.listen(backlog).unwrap();
        socket
    }

    #[test]
    fn lifecycle_checks() {
        assert!(matches!(
            NamedSocket::new(SocketType::Datagram),
            Err(NamedSocketError::UnsupportedType)
        ));
        let socket = NamedSocket::new(SocketType::Stream).unwrap();
        assert_eq!(socket.state(), NamedSocketState::Created);
        socket.bind(UnixAddress::Pathname(b"/run/service".to_vec())).unwrap();
        socket.listen(3).unwrap();
        assert!(matches!(socket.state(), NamedSocketState::Listening { backlog: 3, .. }));
        assert_eq!(
            socket.bind(UnixAddress::Unnamed),
            Err(NamedSocketError::InvalidTransition)
        );
    }

    #[test]
    fn fifo_accept() {
        let listener = listener(SocketType::Stream, 2);
        let first = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let second = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let first_pair = first.connect(&listener, StatusFlags::default(), true).unwrap();
        let second_pair = second.connect(&listener, StatusFlags::default(), true).unwrap();
        assert_eq!(first.state(), NamedSocketState::Connected);
        assert!(listener.readable());
        assert!(Arc::ptr_eq(&listener.accept(true).unwrap(), &first_pair));
        assert!(Arc::ptr_eq(&listener.accept(true).unwrap(), &second_pair));
        assert!(matches!(listener.accept(true), Err(NamedSocketError::WouldBlock)));
    }

    #[test]
    fn nonblocking_rejection() {
        let listener = listener(SocketType::SequencePacket, 1);
        let first = Arc::new(UnixSocketPair::new(SocketType::SequencePacket, StatusFlags::default()).unwrap());
        listener.enqueue(first, true).unwrap();
        let full = Arc::new(UnixSocketPair::new(SocketType::SequencePacket, StatusFlags::default()).unwrap());
        assert_eq!(listener.enqueue(full, true), Err(NamedSocketError::WouldBlock));
        let wrong = Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap());
        assert_eq!(listener.enqueue(wrong, true), Err(NamedSocketError::TypeMismatch));
        assert_eq!(listener.pending(), 1);
    }

    #[test]
    fn blocking_wake() {
        let listener = listener(SocketType::Stream, 1);
        listener
            .enqueue(
                Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap()),
                true,
            )
            .unwrap();
        let producer = listener.clone();
        let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap());
        let expected = pair.clone();
        let thread = thread::spawn(move || producer.enqueue(pair, false));
        assert!(listener.accept(true).is_ok());
        thread.join().unwrap().unwrap();
        assert!(Arc::ptr_eq(&listener.accept(true).unwrap(), &expected));
    }

    #[test]
    fn reservation_rollback() {
        let listener = listener(SocketType::Stream, 1);
        let client = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let reservation = client.reserve_connect(&listener, true).unwrap();
        assert_eq!(client.state(), NamedSocketState::Created);
        assert_eq!(listener.pending(), 0);
        assert!(matches!(listener.accept(true), Err(NamedSocketError::WouldBlock)));

        let competitor = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        assert!(matches!(
            competitor.reserve_connect(&listener, true),
            Err(NamedSocketError::WouldBlock)
        ));
        drop(reservation);

        let replacement = competitor.reserve_connect(&listener, true).unwrap();
        let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap());
        replacement.commit(pair.clone()).unwrap();
        assert_eq!(competitor.state(), NamedSocketState::Connected);
        assert!(Arc::ptr_eq(&listener.accept(true).unwrap(), &pair));
    }

    #[test]
    fn commit_rollback() {
        let listener = listener(SocketType::Stream, 1);
        let client = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let reservation = client.reserve_connect(&listener, true).unwrap();
        let wrong = Arc::new(UnixSocketPair::new(SocketType::SequencePacket, StatusFlags::default()).unwrap());
        assert_eq!(reservation.commit(wrong), Err(NamedSocketError::TypeMismatch));
        assert_eq!(client.state(), NamedSocketState::Created);

        let retry = client.reserve_connect(&listener, true).unwrap();
        let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::default()).unwrap());
        retry.commit(pair).unwrap();
    }

    #[test]
    fn reservation_wake() {
        let listener = listener(SocketType::Stream, 1);
        let first = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let reservation = first.reserve_connect(&listener, true).unwrap();
        let second = Arc::new(NamedSocket::new(SocketType::Stream).unwrap());
        let waiting_listener = listener.clone();
        let waiting_client = second.clone();
        let waiter = thread::spawn(move || waiting_client.reserve_connect(&waiting_listener, false));
        drop(reservation);
        let acquired = waiter.join().unwrap().unwrap();
        drop(acquired);
        assert_eq!(second.state(), NamedSocketState::Created);
    }
}
