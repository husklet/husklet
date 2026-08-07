use crate::{FakeHost, FakeHostError, Fault, ResourceKind};
use hl_network::{SocketConnectStatus, SocketHostError, SocketHostIo, SocketHostReadiness};
use hl_provider::{ProviderTransport, TransportError};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug)]
struct EndpointState {
    bytes: VecDeque<u8>,
    closed: bool,
}

#[derive(Debug)]
struct Endpoint {
    state: Mutex<EndpointState>,
    changed: Condvar,
}

impl Endpoint {
    fn new() -> Self {
        Self {
            state: Mutex::new(EndpointState {
                bytes: VecDeque::new(),
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }
}

pub struct ProviderEndpoint {
    host: FakeHost,
    token: u64,
    endpoint: Arc<Endpoint>,
    maximum_transfer: usize,
}

impl ProviderEndpoint {
    pub fn new(host: FakeHost, maximum_transfer: usize) -> Result<Self, FakeHostError> {
        Ok(Self {
            token: host.allocate("transport", ResourceKind::Transport)?,
            host,
            endpoint: Arc::new(Endpoint::new()),
            maximum_transfer,
        })
    }

    fn transport_error(error: FakeHostError) -> TransportError {
        match error {
            FakeHostError::Fault(Fault::Interrupted) => TransportError::Interrupted,
            FakeHostError::Fault(Fault::WouldBlock) => TransportError::WouldBlock,
            FakeHostError::Closed => TransportError::Closed,
            _ => TransportError::Failed,
        }
    }
}

impl ProviderTransport for ProviderEndpoint {
    fn read(&self, output: &mut [u8]) -> Result<usize, TransportError> {
        let mut state = self
            .endpoint
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.bytes.is_empty() {
            return if state.closed {
                Ok(0)
            } else {
                Err(TransportError::WouldBlock)
            };
        }
        let count = output.len().min(self.maximum_transfer).min(state.bytes.len());
        self.host
            .record("transport", "read", self.token, output.len(), count)
            .map_err(Self::transport_error)?;
        for byte in output.iter_mut().take(count) {
            *byte = state.bytes.pop_front().expect("bounded queue count");
        }
        Ok(count)
    }

    fn write(&self, input: &[u8]) -> Result<usize, TransportError> {
        let count = input.len().min(self.maximum_transfer);
        self.host
            .record("transport", "write", self.token, input.len(), count)
            .map_err(Self::transport_error)?;
        let mut state = self
            .endpoint
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(TransportError::Closed);
        }
        state.bytes.extend(input[..count].iter().copied());
        self.endpoint.changed.notify_all();
        Ok(count)
    }

    fn wait_readable(&self) -> Result<(), TransportError> {
        let state = self
            .endpoint
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.endpoint
                .changed
                .wait_while(state, |state| state.bytes.is_empty() && !state.closed)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(())
    }

    fn wait_writable(&self) -> Result<(), TransportError> {
        Ok(())
    }

    fn shutdown(&self) {
        self.endpoint
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.endpoint.changed.notify_all();
    }
}

impl Drop for ProviderEndpoint {
    fn drop(&mut self) {
        let _ = self.host.release("transport", ResourceKind::Transport, self.token);
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SocketToken(pub u64);

#[derive(Debug)]
pub struct SocketAdapter {
    host: FakeHost,
    sockets: Mutex<BTreeMap<SocketToken, Arc<Endpoint>>>,
    maximum_transfer: usize,
}

impl SocketAdapter {
    #[must_use]
    pub fn new(host: FakeHost, maximum_transfer: usize) -> Self {
        Self {
            host,
            sockets: Mutex::new(BTreeMap::new()),
            maximum_transfer,
        }
    }

    pub fn open(&self) -> Result<SocketToken, FakeHostError> {
        let token = SocketToken(self.host.allocate("socket", ResourceKind::Socket)?);
        self.sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, Arc::new(Endpoint::new()));
        Ok(token)
    }

    fn socket_error(error: FakeHostError) -> SocketHostError {
        match error {
            FakeHostError::Fault(Fault::Interrupted) => SocketHostError::Interrupted,
            FakeHostError::Fault(Fault::WouldBlock) => SocketHostError::WouldBlock,
            _ => SocketHostError::Io,
        }
    }
}

impl SocketHostIo for SocketAdapter {
    type Token = SocketToken;

    fn read(&self, token: Self::Token, output: &mut [u8], _: bool) -> Result<usize, SocketHostError> {
        let sockets = self.sockets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let endpoint = sockets.get(&token).ok_or(SocketHostError::Io)?;
        let mut state = endpoint.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.bytes.is_empty() {
            return Err(SocketHostError::WouldBlock);
        }
        let count = output.len().min(self.maximum_transfer).min(state.bytes.len());
        self.host
            .record("socket", "read", token.0, output.len(), count)
            .map_err(Self::socket_error)?;
        for byte in output.iter_mut().take(count) {
            *byte = state.bytes.pop_front().expect("bounded queue count");
        }
        Ok(count)
    }

    fn write(&self, token: Self::Token, input: &[u8], _: bool) -> Result<usize, SocketHostError> {
        let count = input.len().min(self.maximum_transfer);
        self.host
            .record("socket", "write", token.0, input.len(), count)
            .map_err(Self::socket_error)?;
        let sockets = self.sockets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let endpoint = sockets.get(&token).ok_or(SocketHostError::Io)?;
        endpoint
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bytes
            .extend(input[..count].iter().copied());
        endpoint.changed.notify_all();
        Ok(count)
    }

    fn readiness(&self, token: Self::Token) -> SocketHostReadiness {
        let sockets = self.sockets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sockets.get(&token).map_or(
            SocketHostReadiness {
                error: true,
                ..SocketHostReadiness::default()
            },
            |endpoint| {
                let state = endpoint.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                SocketHostReadiness {
                    readable: !state.bytes.is_empty() || state.closed,
                    priority: false,
                    read_hangup: state.closed,
                    writable: !state.closed,
                    error: false,
                    hangup: state.closed,
                }
            },
        )
    }

    fn start_connect(&self, _: Self::Token, _: bool) -> SocketConnectStatus {
        SocketConnectStatus::Connected
    }

    fn poll_connect(&self, _: Self::Token) -> SocketConnectStatus {
        SocketConnectStatus::Connected
    }

    fn cancel(&self, token: Self::Token) {
        if let Some(endpoint) = self
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
        {
            endpoint
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .closed = true;
            endpoint.changed.notify_all();
        }
    }

    fn close(&self, token: Self::Token) {
        if self
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&token)
            .is_some()
        {
            let _ = self.host.release("socket", ResourceKind::Socket, token.0);
        }
    }
}
