use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::listener::ListenerQueue;
use crate::{AcceptError, AcceptedToken, SocketAddress};
use hl_descriptor::{
    CancellationNotification, DescriptorFlags, ObjectError, ObjectKind, OpenFileDescription, OperationCancellation,
    Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription, StatusFlags,
};

#[derive(Debug, Default)]
struct WaitGate {
    generation: Mutex<u64>,
    wake: Condvar,
}
impl WaitGate {
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn notify(&self) {
        let mut value = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        *value = value.wrapping_add(1);
        drop(value);
        self.wake.notify_all();
    }
    fn wait(&self, observed: u64) {
        let mut value = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        while *value == observed {
            value = self.wake.wait(value).unwrap_or_else(|e| e.into_inner());
        }
    }
}
impl ReadinessObserver for WaitGate {
    fn readiness_changed(&self) {
        self.notify();
    }
}
impl CancellationNotification for WaitGate {
    fn notify(&self) {
        WaitGate::notify(self);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketHostError {
    WouldBlock,
    Interrupted,
    Canceled,
    BrokenPipe,
    DestinationRequired,
    MessageTooLarge,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    ShutDown,
    HostUnreachable,
    NetworkUnreachable,
    NetworkDown,
    NetworkReset,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketConnectError {
    InProgress,
    Already,
    Connected,
    Interrupted,
    Refused,
    TimedOut,
    Canceled,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketConnectStatus {
    Idle,
    Pending,
    Connected,
    Failed(SocketConnectError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SocketHostReadiness {
    pub readable: bool,
    pub priority: bool,
    pub read_hangup: bool,
    pub writable: bool,
    pub error: bool,
    pub hangup: bool,
}

pub trait SocketHostIo: Debug + Send + Sync + 'static {
    type Token: Copy + Debug + Send + Sync + 'static;

    fn read(&self, token: Self::Token, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError>;

    fn peek(&self, _token: Self::Token, _output: &mut [u8]) -> Result<usize, SocketHostError> {
        Err(SocketHostError::Io)
    }

    fn write(&self, token: Self::Token, input: &[u8], nonblocking: bool) -> Result<usize, SocketHostError>;

    fn readiness(&self, token: Self::Token) -> SocketHostReadiness;
    fn start_connect(&self, token: Self::Token, nonblocking: bool) -> SocketConnectStatus;
    fn poll_connect(&self, token: Self::Token) -> SocketConnectStatus;
    fn attach_readiness(&self, _token: Self::Token, _observer: std::sync::Weak<dyn ReadinessObserver>) {}
    fn detach_readiness(&self, _token: Self::Token) {}
    /// Wakes every operation parked on this token. Must be idempotent.
    fn cancel(&self, token: Self::Token);
    fn close(&self, token: Self::Token);
}

pub struct SocketDescription<H: SocketHostIo> {
    host: Arc<H>,
    token: Mutex<Option<H::Token>>,
    flags: AtomicU32,
    retired: AtomicBool,
    readiness: ReadinessRegistry,
    connect: Mutex<SocketConnectStatus>,
    listener: ListenerQueue<H>,
}

impl<H: SocketHostIo> Debug for SocketDescription<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketDescription")
            .field("retired", &self.retired.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl<H: SocketHostIo> SocketDescription<H> {
    pub fn bind_readiness(self: &Arc<Self>) {
        let Ok(token) = self.token() else { return };
        let observer: Arc<dyn ReadinessObserver> = self.clone();
        self.host.attach_readiness(token, Arc::downgrade(&observer));
    }
    pub fn read_with(&self, output: &mut [u8], nonblocking: bool) -> Result<usize, ObjectError> {
        self.host.read(self.token()?, output, nonblocking).map_err(Self::error)
    }

    pub fn write_with(&self, input: &[u8], nonblocking: bool) -> Result<usize, ObjectError> {
        self.host.write(self.token()?, input, nonblocking).map_err(Self::error)
    }

    #[must_use]
    pub fn new(host: Arc<H>, token: H::Token, flags: StatusFlags) -> Self {
        Self::restored(host, token, flags, SocketConnectStatus::Idle)
    }

    #[must_use]
    pub fn restored(host: Arc<H>, token: H::Token, flags: StatusFlags, connect: SocketConnectStatus) -> Self {
        Self {
            host: Arc::clone(&host),
            token: Mutex::new(Some(token)),
            flags: AtomicU32::new(flags.bits()),
            retired: AtomicBool::new(false),
            readiness: ReadinessRegistry::new(),
            connect: Mutex::new(connect),
            listener: ListenerQueue::new(Arc::clone(&host)),
        }
    }

    fn token(&self) -> Result<H::Token, ObjectError> {
        if self.retired.load(Ordering::Acquire) {
            return Err(ObjectError::Retired);
        }
        self.token
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .ok_or(ObjectError::Retired)
    }

    fn nonblocking(&self) -> bool {
        self.flags.load(Ordering::Acquire) & StatusFlags::NONBLOCKING != 0
    }

    fn error(error: SocketHostError) -> ObjectError {
        match error {
            SocketHostError::WouldBlock => ObjectError::WouldBlock,
            SocketHostError::Interrupted => ObjectError::Interrupted,
            SocketHostError::Canceled => ObjectError::Canceled,
            SocketHostError::BrokenPipe => ObjectError::BrokenPipe,
            SocketHostError::DestinationRequired
            | SocketHostError::MessageTooLarge
            | SocketHostError::ConnectionReset
            | SocketHostError::ConnectionAborted
            | SocketHostError::NotConnected
            | SocketHostError::ShutDown
            | SocketHostError::HostUnreachable
            | SocketHostError::NetworkUnreachable
            | SocketHostError::NetworkDown
            | SocketHostError::NetworkReset => ObjectError::Io,
            SocketHostError::Io => ObjectError::Io,
        }
    }

    pub fn notify_readiness(&self) {
        self.readiness.notify();
    }

    pub fn wait_readable(&self, cancellation: &dyn OperationCancellation) -> Result<(), ObjectError> {
        self.wait_for(Readiness::READ, cancellation)
    }

    pub fn wait_writable(&self, cancellation: &dyn OperationCancellation) -> Result<(), ObjectError> {
        self.wait_for(Readiness::WRITE, cancellation)
    }

    pub fn observe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.readiness.subscribe(observer)
    }

    fn wait_for(&self, interest: u32, cancellation: &dyn OperationCancellation) -> Result<(), ObjectError> {
        let gate = Arc::new(WaitGate::default());
        let _ready = self.subscribe_readiness(gate.clone())?;
        let _cancel = cancellation.subscribe(gate.clone());
        loop {
            let observed = gate.generation();
            if self.readiness(Readiness::from_bits(interest)).contains(interest) {
                return Ok(());
            }
            if cancellation.interrupted() {
                return Err(ObjectError::Interrupted);
            }
            gate.wait(observed);
        }
    }

    pub fn connect(&self) -> Result<(), SocketConnectError> {
        let token = self.token().map_err(|_| SocketConnectError::Canceled)?;
        let mut state = self.connect.lock().unwrap_or_else(|error| error.into_inner());
        match *state {
            SocketConnectStatus::Pending => return Err(SocketConnectError::Already),
            SocketConnectStatus::Connected => return Err(SocketConnectError::Connected),
            SocketConnectStatus::Idle => {}
            SocketConnectStatus::Failed(error) => return Err(error),
        }
        let status = self.host.start_connect(token, self.nonblocking());
        *state = status;
        match status {
            SocketConnectStatus::Idle => Err(SocketConnectError::Io),
            SocketConnectStatus::Pending => Err(SocketConnectError::InProgress),
            SocketConnectStatus::Connected => Ok(()),
            SocketConnectStatus::Failed(error) => Err(error),
        }
    }

    pub fn connect_with_cancellation(
        &self,
        cancellation: &dyn OperationCancellation,
    ) -> Result<(), SocketConnectError> {
        match self.connect() {
            Err(SocketConnectError::InProgress) if !self.nonblocking() => {}
            result => return result,
        }
        let gate = Arc::new(WaitGate::default());
        let _ready = self
            .subscribe_readiness(gate.clone())
            .map_err(|_| SocketConnectError::Canceled)?;
        let _cancel = cancellation.subscribe(gate.clone());
        loop {
            let observed = gate.generation();
            match self.poll_connect() {
                Err(SocketConnectError::InProgress) => {}
                result => return result,
            }
            if cancellation.interrupted() {
                return Err(SocketConnectError::Interrupted);
            }
            gate.wait(observed);
        }
    }

    pub fn poll_connect(&self) -> Result<(), SocketConnectError> {
        let token = self.token().map_err(|_| SocketConnectError::Canceled)?;
        let mut state = self.connect.lock().unwrap_or_else(|error| error.into_inner());
        if *state == SocketConnectStatus::Pending {
            *state = self.host.poll_connect(token);
            if *state != SocketConnectStatus::Pending {
                self.readiness.notify();
            }
        }
        match *state {
            SocketConnectStatus::Idle => Err(SocketConnectError::Io),
            SocketConnectStatus::Pending => Err(SocketConnectError::InProgress),
            SocketConnectStatus::Connected => Ok(()),
            SocketConnectStatus::Failed(error) => Err(error),
        }
    }

    pub fn take_connect_error(&self) -> Option<SocketConnectError> {
        let mut state = self.connect.lock().unwrap_or_else(|error| error.into_inner());
        let SocketConnectStatus::Failed(error) = *state else {
            return None;
        };
        *state = SocketConnectStatus::Idle;
        Some(error)
    }

    /// Refreshes asynchronous connect state without consuming its latched error.
    pub fn connect_status(&self) -> SocketConnectStatus {
        match self.poll_connect() {
            Ok(()) | Err(_) => {}
        }
        *self.connect.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Commits delivery of a previously observed connect error.
    ///
    /// A changed state is left untouched, so callers may copy the observed
    /// value to guest memory before committing the one-shot consumption.
    pub fn commit_connect_error(&self, observed: SocketConnectError) {
        let mut state = self.connect.lock().unwrap_or_else(|error| error.into_inner());
        if *state == SocketConnectStatus::Failed(observed) {
            *state = SocketConnectStatus::Idle;
        }
    }

    pub fn listen(&self, backlog: usize) {
        self.listener.set_backlog(backlog);
    }

    pub fn publish_accepted(
        &self,
        token: H::Token,
        local: SocketAddress,
        peer: SocketAddress,
    ) -> Result<(), AcceptError> {
        let result = self.listener.publish(AcceptedToken { token, local, peer });
        if result.is_ok() {
            self.readiness.notify();
        }
        result
    }

    pub fn accept(&self, nonblocking: bool, close_on_exec: bool) -> Result<AcceptedDescription<H>, AcceptError> {
        let accepted = self.listener.accept(nonblocking)?;
        let flags = StatusFlags::from_bits(if nonblocking { StatusFlags::NONBLOCKING } else { 0 });
        Ok(AcceptedDescription {
            description: Arc::new(Self::new(Arc::clone(&self.host), accepted.token, flags)),
            descriptor_flags: DescriptorFlags::from_bits(if close_on_exec {
                DescriptorFlags::CLOSE_ON_EXEC
            } else {
                0
            }),
            local: accepted.local,
            peer: accepted.peer,
        })
    }

    #[cfg(test)]
    pub fn accept_waiting(&self) -> u64 {
        self.listener.waiting()
    }

    #[cfg(test)]
    pub fn notify_accept_spurious(&self) {
        self.listener.notify_spurious();
    }
}

impl<H: SocketHostIo> ReadinessObserver for SocketDescription<H> {
    fn readiness_changed(&self) {
        self.readiness.notify();
    }
}

impl<H: SocketHostIo> OpenFileDescription for SocketDescription<H> {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Socket
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read_with(output, self.nonblocking())
    }
    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        let mut byte = [0_u8; 1];
        match self.host.peek(self.token()?, &mut byte[..maximum.min(1)]) {
            Ok(count) => Ok(Some(count)),
            Err(SocketHostError::WouldBlock) if !self.nonblocking() => Ok(Some(1)),
            Err(error) => Err(Self::error(error)),
        }
    }
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if self.nonblocking() {
            return self.read(output);
        }
        let gate = Arc::new(WaitGate::default());
        let _ready = self.subscribe_readiness(gate.clone())?;
        let _cancel = cancellation.subscribe(gate.clone());
        loop {
            let observed = gate.generation();
            match self.read_with(output, true) {
                Err(ObjectError::WouldBlock) => {}
                result => return result,
            }
            if cancellation.interrupted() {
                return Err(ObjectError::Interrupted);
            }
            gate.wait(observed);
        }
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.write_with(input, self.nonblocking())
    }
    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if self.nonblocking() {
            return self.write(input);
        }
        let gate = Arc::new(WaitGate::default());
        let _ready = self.subscribe_readiness(gate.clone())?;
        let _cancel = cancellation.subscribe(gate.clone());
        loop {
            let observed = gate.generation();
            match self.write_with(input, true) {
                Err(ObjectError::WouldBlock) => {}
                result => return result,
            }
            if cancellation.interrupted() {
                return Err(ObjectError::Interrupted);
            }
            gate.wait(observed);
        }
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.token()?;
        self.flags.store(flags.bits(), Ordering::Release);
        Ok(())
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        let Ok(token) = self.token() else {
            return Readiness::from_bits(Readiness::HANGUP | Readiness::ERROR);
        };
        let connect = self.connect_status();
        let value = self.host.readiness(token);
        let listener_readable = self.listener.readable();
        let reported = Readiness::from_bits(
            (if value.readable || listener_readable {
                Readiness::READ
            } else {
                0
            }) | (if value.priority { Readiness::PRIORITY } else { 0 })
                | (if value.read_hangup { Readiness::READ_HANGUP } else { 0 }) | (if value.writable
                || matches!(connect, SocketConnectStatus::Connected | SocketConnectStatus::Failed(_))
            {
                Readiness::WRITE
            } else {
                0
            }) | (if value.error || matches!(connect, SocketConnectStatus::Failed(_)) {
                Readiness::ERROR
            } else {
                0
            }) | (if value.hangup { Readiness::HANGUP } else { 0 }),
        );
        let requested = interests.bits();
        let ordinary = if requested == 0 {
            // An empty mask is the domain's full-state observation used by
            // descriptor probes. It must not consume listener queue state.
            reported.bits()
        } else {
            reported.bits() & requested
        };
        Readiness::from_bits(ordinary | (reported.bits() & (Readiness::ERROR | Readiness::HANGUP)))
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.readiness.subscribe(observer)
    }

    fn retire(&self) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(token) = *self.token.lock().unwrap_or_else(|error| error.into_inner()) {
            self.host.cancel(token);
        }
        self.listener.cancel_and_drain();
        self.readiness.close();
    }

    fn close(&self) {
        if let Some(token) = self.token.lock().unwrap_or_else(|error| error.into_inner()).take() {
            self.host.detach_readiness(token);
            self.host.close(token);
        }
    }
}

pub struct AcceptedDescription<H: SocketHostIo> {
    pub description: Arc<SocketDescription<H>>,
    pub descriptor_flags: DescriptorFlags,
    pub local: SocketAddress,
    pub peer: SocketAddress,
}
