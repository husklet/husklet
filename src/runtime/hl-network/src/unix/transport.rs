use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_descriptor::DescriptorTable;
use hl_sync::WaitQueue;

use crate::{
    ControlError, ControlMessage, ReceiveControl, SenderCredentials, SocketConnectStatus, SocketDescription,
    SocketHostError, SocketHostIo, SocketHostReadiness, SocketType, UnixAddress, UnixMessageQueue,
};

pub(super) const UNIX_BUFFER_DEFAULT: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Invalid,
    WouldBlock,
    BrokenPipe,
    Canceled,
    Control(ControlError),
}
pub type UnixTransportError = TransportError;

pub(super) struct EndpointState {
    pub(super) incoming: VecDeque<Vec<u8>>,
    pub(super) bytes: usize,
    pub(super) ancillary_bytes: usize,
    pub(super) peer_write_shutdown: bool,
    pub(super) read_shutdown: bool,
    pub(super) write_shutdown: bool,
    pub(super) closed: bool,
    pub(super) canceled: bool,
}

impl EndpointState {
    pub(super) fn new() -> Self {
        Self {
            incoming: VecDeque::new(),
            bytes: 0,
            ancillary_bytes: 0,
            peer_write_shutdown: false,
            read_shutdown: false,
            write_shutdown: false,
            closed: false,
            canceled: false,
        }
    }
    fn read_into(&mut self, output: &mut [u8], stream: bool) -> Option<usize> {
        let mut data = self.incoming.pop_front()?;
        let count = output.len().min(data.len());
        output[..count].copy_from_slice(&data[..count]);
        self.bytes -= if stream { count } else { data.len() };
        if stream && count < data.len() {
            data.drain(..count);
            self.incoming.push_front(data);
        }
        Some(count)
    }
    fn peek_into(&self, output: &mut [u8]) -> Option<usize> {
        let data = self.incoming.front()?;
        let count = output.len().min(data.len());
        output[..count].copy_from_slice(&data[..count]);
        Some(count)
    }
    fn enqueue(&mut self, input: &[u8], stream: bool) {
        if stream {
            if let Some(buffer) = self.incoming.back_mut() {
                buffer.extend_from_slice(input);
                self.bytes += input.len();
                return;
            }
        }
        self.incoming.push_back(input.to_vec());
        self.bytes += input.len();
    }
}

pub(super) struct State {
    pub(super) endpoints: [EndpointState; 2],
}
pub(super) type UnixState = State;

pub struct SocketHost {
    pub(super) socket_type: SocketType,
    pub(super) capacity: usize,
    pub(super) state: Mutex<UnixState>,
    pub(super) wake: Condvar,
    pub(super) message_wait: [Arc<WaitQueue>; 2],
    pub(super) readiness: Mutex<[Option<Weak<dyn hl_descriptor::ReadinessObserver>>; 2]>,
}
pub type UnixSocketHost = SocketHost;

impl Debug for UnixSocketHost {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnixSocketHost")
            .field("socket_type", &self.socket_type)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl UnixSocketHost {
    pub(super) fn notify(&self) {
        self.wake.notify_all();
        for queue in &self.message_wait {
            queue.notify_all();
        }
        let observers = self
            .readiness
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|observer| observer.as_ref().and_then(Weak::upgrade))
            .collect::<Vec<_>>();
        for observer in observers {
            observer.readiness_changed();
        }
    }
    fn peer(token: usize) -> usize {
        1 - token
    }

    pub(super) fn wait<'state>(
        &self,
        state: std::sync::MutexGuard<'state, UnixState>,
    ) -> std::sync::MutexGuard<'state, UnixState> {
        self.wake.wait(state).unwrap_or_else(|error| error.into_inner())
    }

    fn shutdown(&self, token: usize, read: bool, write: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if read {
            state.endpoints[token].read_shutdown = true;
            state.endpoints[token].incoming.clear();
            state.endpoints[token].bytes = 0;
        }
        if write {
            state.endpoints[token].write_shutdown = true;
            if self.socket_type != SocketType::Datagram {
                state.endpoints[Self::peer(token)].peer_write_shutdown = true;
            }
        }
        drop(state);
        if read {
            self.message_wait[token].notify_all();
        }
        if write {
            self.message_wait[Self::peer(token)].notify_all();
        }
        self.notify();
    }

    fn peek(&self, token: usize, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let endpoint = &state.endpoints[token];
            if endpoint.canceled {
                return Err(SocketHostError::Canceled);
            }
            if endpoint.read_shutdown {
                return Ok(0);
            }
            if let Some(count) = endpoint.peek_into(output) {
                return Ok(count);
            }
            if endpoint.peer_write_shutdown {
                return Ok(0);
            }
            if nonblocking {
                return Err(SocketHostError::WouldBlock);
            }
            state = self.wait(state);
        }
    }

    fn reserve_message(&self, token: usize, length: usize, nonblocking: bool) -> Result<(), UnixTransportError> {
        if length > self.capacity {
            return Err(UnixTransportError::Control(ControlError::TooBig));
        }
        let peer = Self::peer(token);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if state.endpoints[token].canceled {
                return Err(UnixTransportError::Canceled);
            }
            if state.endpoints[token].write_shutdown
                || state.endpoints[peer].closed
                || state.endpoints[peer].read_shutdown
            {
                return Err(UnixTransportError::BrokenPipe);
            }
            let used = state.endpoints[peer]
                .bytes
                .saturating_add(state.endpoints[peer].ancillary_bytes);
            if length <= self.capacity.saturating_sub(used) {
                state.endpoints[peer].ancillary_bytes += length;
                return Ok(());
            }
            if nonblocking {
                return Err(UnixTransportError::WouldBlock);
            }
            state = self.wait(state);
        }
    }

    fn release_message(&self, token: usize, length: usize) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.endpoints[token].ancillary_bytes = state.endpoints[token].ancillary_bytes.saturating_sub(length);
        drop(state);
        self.notify();
    }
}

impl SocketHostIo for UnixSocketHost {
    type Token = usize;

    fn read(&self, token: usize, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            let endpoint = &mut state.endpoints[token];
            if endpoint.canceled {
                return Err(SocketHostError::Canceled);
            }
            if endpoint.read_shutdown {
                return Ok(0);
            }
            if let Some(count) = endpoint.read_into(output, self.socket_type == SocketType::Stream) {
                drop(state);
                self.notify();
                return Ok(count);
            }
            if endpoint.peer_write_shutdown {
                return Ok(0);
            }
            if nonblocking {
                return Err(SocketHostError::WouldBlock);
            }
            state = self.wait(state);
        }
    }

    fn peek(&self, token: usize, output: &mut [u8]) -> Result<usize, SocketHostError> {
        UnixSocketHost::peek(self, token, output, true)
    }

    fn write(&self, token: usize, input: &[u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        let peer = Self::peer(token);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if state.endpoints[token].canceled {
                return Err(SocketHostError::Canceled);
            }
            if state.endpoints[token].write_shutdown
                || state.endpoints[peer].closed
                || state.endpoints[peer].read_shutdown
            {
                return Err(SocketHostError::BrokenPipe);
            }
            let available = self.capacity.saturating_sub(state.endpoints[peer].bytes);
            let count = if self.socket_type == SocketType::Stream {
                available.min(input.len())
            } else if input.len() <= available {
                input.len()
            } else {
                0
            };
            if count > 0 || input.is_empty() {
                state.endpoints[peer].enqueue(&input[..count], self.socket_type == SocketType::Stream);
                drop(state);
                self.notify();
                return Ok(count);
            }
            if nonblocking {
                return Err(SocketHostError::WouldBlock);
            }
            state = self.wait(state);
        }
    }

    fn readiness(&self, token: usize) -> SocketHostReadiness {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let endpoint = &state.endpoints[token];
        let peer = &state.endpoints[Self::peer(token)];
        SocketHostReadiness {
            readable: !endpoint.incoming.is_empty() || endpoint.ancillary_bytes > 0 || endpoint.peer_write_shutdown,
            priority: false,
            read_hangup: endpoint.peer_write_shutdown,
            writable: !endpoint.write_shutdown
                && !peer.closed
                && !peer.read_shutdown
                && peer.bytes.saturating_add(peer.ancillary_bytes) < self.capacity,
            error: endpoint.canceled,
            hangup: endpoint.peer_write_shutdown || peer.closed,
        }
    }

    fn start_connect(&self, _token: usize, _nonblocking: bool) -> SocketConnectStatus {
        SocketConnectStatus::Connected
    }

    fn poll_connect(&self, _token: usize) -> SocketConnectStatus {
        SocketConnectStatus::Connected
    }

    fn attach_readiness(&self, token: usize, observer: Weak<dyn hl_descriptor::ReadinessObserver>) {
        self.readiness.lock().unwrap_or_else(|error| error.into_inner())[token] = Some(observer);
    }

    fn detach_readiness(&self, token: usize) {
        self.readiness.lock().unwrap_or_else(|error| error.into_inner())[token] = None;
    }

    fn cancel(&self, token: usize) {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).endpoints[token].canceled = true;
        self.message_wait[token].notify_all();
        self.notify();
    }

    fn close(&self, token: usize) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.endpoints[token].closed = true;
        if self.socket_type != SocketType::Datagram {
            state.endpoints[Self::peer(token)].peer_write_shutdown = true;
        }
        drop(state);
        self.message_wait[token].notify_all();
        self.message_wait[Self::peer(token)].notify_all();
        self.notify();
    }
}

pub struct SocketEndpoint {
    pub(super) host: Arc<UnixSocketHost>,
    pub(super) token: usize,
    pub(super) address: UnixAddress,
    pub(super) ancillary: [Arc<UnixMessageQueue>; 2],
    pub description: Arc<SocketDescription<UnixSocketHost>>,
}
pub type UnixSocketEndpoint = SocketEndpoint;

impl UnixSocketEndpoint {
    #[must_use]
    pub fn readable_bytes(&self) -> usize {
        self.host
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .endpoints[self.token]
            .bytes
    }

    pub fn peek(&self, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError> {
        self.host.peek(self.token, output, nonblocking)
    }

    pub fn receive_record(
        &self,
        output: &mut [u8],
        nonblocking: bool,
        peek: bool,
    ) -> Result<(usize, usize), SocketHostError> {
        self.host.receive_record(self.token, output, nonblocking, peek)
    }

    pub fn shutdown(&self, read: bool, write: bool) {
        self.host.shutdown(self.token, read, write);
    }

    pub fn set_passcred(&self, enabled: bool) {
        self.ancillary[self.token].set_passcred(enabled);
    }

    #[must_use]
    pub fn passcred(&self) -> bool {
        self.ancillary[self.token].passcred()
    }

    pub fn send_message(
        &self,
        sender: &DescriptorTable,
        payload: Vec<u8>,
        controls: Vec<ControlMessage>,
        credentials: Option<SenderCredentials>,
        nonblocking: bool,
    ) -> Result<(), UnixTransportError> {
        self.send_message_with(sender, payload, controls, || credentials, nonblocking)
    }

    pub fn send_message_with<F>(
        &self,
        sender: &DescriptorTable,
        payload: Vec<u8>,
        controls: Vec<ControlMessage>,
        credentials: F,
        nonblocking: bool,
    ) -> Result<(), UnixTransportError>
    where
        F: FnOnce() -> Option<SenderCredentials>,
    {
        let length = payload.len();
        self.host.reserve_message(self.token, length, nonblocking)?;
        let result = self.ancillary[UnixSocketHost::peer(self.token)]
            .send_authenticated(sender, payload, controls, credentials())
            .map_err(UnixTransportError::Control);
        if result.is_err() {
            self.host.release_message(UnixSocketHost::peer(self.token), length);
        } else {
            self.host.notify();
        }
        result
    }

    pub fn receive_message(
        &self,
        receiver: &DescriptorTable,
        descriptor_capacity: usize,
        close_on_exec: bool,
    ) -> Result<Option<(Vec<u8>, ReceiveControl)>, UnixTransportError> {
        let received = self.ancillary[self.token]
            .receive(receiver, descriptor_capacity, close_on_exec)
            .map_err(UnixTransportError::Control)?;
        if let Some((payload, _)) = &received {
            self.host.release_message(self.token, payload.len());
        }
        Ok(received)
    }

    pub fn receive_message_transactional<F>(
        &self,
        receiver: &DescriptorTable,
        control_capacity: usize,
        close_on_exec: bool,
        peek: bool,
        copyout: F,
    ) -> Result<Option<ReceiveControl>, UnixTransportError>
    where
        F: FnOnce(&[u8], &ReceiveControl) -> Result<(), ControlError>,
    {
        let length = std::cell::Cell::new(None);
        let received = if peek {
            self.ancillary[self.token].peek_transactional_capacity(receiver, control_capacity, close_on_exec, copyout)
        } else {
            self.ancillary[self.token].receive_observed(
                receiver,
                control_capacity,
                close_on_exec,
                |payload_length| length.set(Some(payload_length)),
                copyout,
            )
        }
        .map_err(UnixTransportError::Control);
        if let Some(length) = length.get() {
            self.host.release_message(self.token, length);
        }
        received
    }

    #[must_use]
    pub fn address(&self) -> &UnixAddress {
        &self.address
    }
}
