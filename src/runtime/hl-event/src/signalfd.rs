use std::sync::{Arc, Condvar, Mutex, Weak};

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessRegistry,
    ReadinessSubscription, StatusFlags,
};
#[path = "signalfd_prepared.rs"]
mod prepared;

pub const SIGNALFD_RECORD_SIZE: usize = 128;
const SIGNALFD_MODE: u32 = 0o100_600;
const SIGKILL: u32 = 9;
const SIGSTOP: u32 = 19;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct SignalMask(u64);

impl SignalMask {
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits & !(Self::signal_bit(SIGKILL) | Self::signal_bit(SIGSTOP)))
    }
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }
    #[must_use]
    pub const fn contains(self, signal: u32) -> bool {
        signal >= 1 && signal <= 64 && self.0 & Self::signal_bit(signal) != 0
    }
    const fn signal_bit(signal: u32) -> u64 {
        1_u64 << (signal - 1)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct SignalFdFlags(u32);

impl SignalFdFlags {
    pub const NONBLOCKING: u32 = 0x800;
    pub const CLOSE_ON_EXEC: u32 = 0x8_0000;
    const ALLOWED: u32 = Self::NONBLOCKING | Self::CLOSE_ON_EXEC;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn closes_on_exec(self) -> bool {
        self.0 & Self::CLOSE_ON_EXEC != 0
    }
    const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }
    const fn nonblocking(self) -> bool {
        self.0 & Self::NONBLOCKING != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalQueueError {
    Interrupted,
    Canceled,
    Failed,
}

/// Notification installed by a signal consumer.
pub trait SignalObserver: Send + Sync {
    fn signal_available(&self);
}

/// Quiescent registration returned by a task-owned signal queue.
pub trait SignalSubscription: Send + Sync {
    /// Stops future callbacks and waits for callbacks already in flight.
    fn quiesce(&self);
}

/// Task-owned pending-signal capability consumed by signalfd.
///
/// The provider owns standard-signal coalescing, realtime priority/FIFO
/// ordering, process/thread routing, and removal from the pending set.
pub trait SignalQueue: std::fmt::Debug + Send + Sync {
    fn dequeue(&self, mask: SignalMask) -> Result<Option<SignalInfo>, SignalQueueError>;

    fn has_pending(&self, mask: SignalMask) -> bool;

    fn subscribe(&self, observer: Arc<dyn SignalObserver>) -> Result<Box<dyn SignalSubscription>, SignalQueueError>;

    fn prepare(&self, _mask: SignalMask) -> Result<Option<Box<dyn PreparedSignalSelection>>, SignalQueueError> {
        Err(SignalQueueError::Failed)
    }
    fn prepare_context(
        &self,
        mask: SignalMask,
        _actor: hl_descriptor::OperationActor,
    ) -> Result<Option<Box<dyn PreparedSignalSelection>>, SignalQueueError> {
        self.prepare(mask)
    }
}

pub trait PreparedSignalSelection: Send {
    fn info(&self) -> SignalInfo;
    fn commit(self: Box<Self>) -> Result<bool, SignalQueueError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SignalInfo {
    pub signal: u32,
    pub error: i32,
    pub code: i32,
    pub process_id: u32,
    pub user_id: u32,
    pub file_descriptor: i32,
    pub thread_id: u32,
    pub band: u32,
    pub overrun: u32,
    pub trap_number: u32,
    pub status: i32,
    pub integer: i32,
    pub pointer: u64,
    pub user_time: u64,
    pub system_time: u64,
    pub address: u64,
    pub address_lsb: u16,
    pub syscall: i32,
    pub call_address: u64,
    pub architecture: u32,
}

impl SignalInfo {
    #[must_use]
    pub fn encode(self) -> [u8; SIGNALFD_RECORD_SIZE] {
        let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
        Self::put_u32(&mut record, 0, self.signal);
        Self::put_i32(&mut record, 4, self.error);
        Self::put_i32(&mut record, 8, self.code);
        Self::put_u32(&mut record, 12, self.process_id);
        Self::put_u32(&mut record, 16, self.user_id);
        Self::put_i32(&mut record, 20, self.file_descriptor);
        Self::put_u32(&mut record, 24, self.thread_id);
        Self::put_u32(&mut record, 28, self.band);
        Self::put_u32(&mut record, 32, self.overrun);
        Self::put_u32(&mut record, 36, self.trap_number);
        Self::put_i32(&mut record, 40, self.status);
        Self::put_i32(&mut record, 44, self.integer);
        Self::put_u64(&mut record, 48, self.pointer);
        Self::put_u64(&mut record, 56, self.user_time);
        Self::put_u64(&mut record, 64, self.system_time);
        Self::put_u64(&mut record, 72, self.address);
        record[80..82].copy_from_slice(&self.address_lsb.to_le_bytes());
        Self::put_i32(&mut record, 84, self.syscall);
        Self::put_u64(&mut record, 88, self.call_address);
        Self::put_u32(&mut record, 96, self.architecture);
        record
    }
    fn put_i32(output: &mut [u8], offset: usize, value: i32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(output: &mut [u8], offset: usize, value: u32) {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(output: &mut [u8], offset: usize, value: u64) {
        output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalFdError {
    InvalidArgument,
    WouldBlock,
    Interrupted,
    Canceled,
    Retired,
    QueueFailed,
}

impl SignalFdError {
    const fn object_error(self) -> ObjectError {
        match self {
            Self::InvalidArgument => ObjectError::InvalidArgument,
            Self::WouldBlock => ObjectError::WouldBlock,
            Self::Interrupted => ObjectError::Interrupted,
            Self::Canceled => ObjectError::Canceled,
            Self::Retired => ObjectError::Retired,
            Self::QueueFailed => ObjectError::Io,
        }
    }
}

impl From<SignalQueueError> for SignalFdError {
    fn from(error: SignalQueueError) -> Self {
        match error {
            SignalQueueError::Interrupted => Self::Interrupted,
            SignalQueueError::Canceled => Self::Canceled,
            SignalQueueError::Failed => Self::QueueFailed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalFdSnapshot {
    pub mask: SignalMask,
    pub nonblocking: bool,
}

pub type SignalFdStatus = crate::EventStatus;

struct SignalFdState {
    mask: SignalMask,
    nonblocking: bool,
    retired: bool,
    subscription: Option<Box<dyn SignalSubscription>>,
}

struct SignalFdInner {
    queue: Arc<dyn SignalQueue>,
    state: Mutex<SignalFdState>,
    changed: Condvar,
    readiness: ReadinessRegistry,
}

struct QueueObserver {
    inner: Weak<SignalFdInner>,
}

impl SignalObserver for QueueObserver {
    fn signal_available(&self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.retired {
            inner.changed.notify_all();
        }
        drop(state);
        inner.readiness.notify();
    }
}

/// A Linux signalfd open-file-description.
#[derive(Clone)]
pub struct SignalFd {
    inner: Arc<SignalFdInner>,
}

impl std::fmt::Debug for SignalFd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignalFd")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl SignalFd {
    pub fn new(mask: SignalMask, flags: SignalFdFlags, queue: Arc<dyn SignalQueue>) -> Result<Self, SignalFdError> {
        if !flags.valid() {
            return Err(SignalFdError::InvalidArgument);
        }
        let inner = Arc::new(SignalFdInner {
            queue: Arc::clone(&queue),
            state: Mutex::new(SignalFdState {
                mask,
                nonblocking: flags.nonblocking(),
                retired: false,
                subscription: None,
            }),
            changed: Condvar::new(),
            readiness: ReadinessRegistry::new(),
        });
        let observer = Arc::new(QueueObserver {
            inner: Arc::downgrade(&inner),
        });
        let subscription = queue.subscribe(observer)?;
        inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .subscription = Some(subscription);
        Ok(Self { inner })
    }

    pub fn from_snapshot(snapshot: SignalFdSnapshot, queue: Arc<dyn SignalQueue>) -> Result<Self, SignalFdError> {
        let flags = if snapshot.nonblocking {
            SignalFdFlags::NONBLOCKING
        } else {
            0
        };
        Self::new(snapshot.mask, SignalFdFlags::from_bits(flags), queue)
    }

    pub fn set_mask(&self, mask: SignalMask) -> Result<(), SignalFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(SignalFdError::Retired);
        }
        state.mask = mask;
        self.inner.changed.notify_all();
        drop(state);
        self.inner.readiness.notify();
        Ok(())
    }

    pub fn read(&self, output: &mut [u8]) -> Result<usize, SignalFdError> {
        if output.len() < SIGNALFD_RECORD_SIZE {
            return Err(SignalFdError::InvalidArgument);
        }
        let capacity = output.len() / SIGNALFD_RECORD_SIZE;
        let first = self.dequeue_first()?;
        Self::write_record(output, 0, first);
        let mut count = 1;
        while count < capacity {
            let mask = self.current_mask()?;
            let Some(info) = self.inner.queue.dequeue(mask)? else {
                break;
            };
            Self::write_record(output, count, info);
            count += 1;
        }
        self.inner.readiness.notify();
        Ok(count * SIGNALFD_RECORD_SIZE)
    }

    #[must_use]
    pub fn readiness(&self, interests: Readiness) -> Readiness {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = if state.retired {
            Readiness::ERROR
        } else if self.inner.queue.has_pending(state.mask) {
            Readiness::READ
        } else {
            0
        };
        Readiness::from_bits(ready & (interests.bits() | Readiness::ERROR | Readiness::HANGUP))
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), SignalFdError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return Err(SignalFdError::Retired);
        }
        state.nonblocking = nonblocking;
        self.inner.changed.notify_all();
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> SignalFdSnapshot {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SignalFdSnapshot {
            mask: state.mask,
            nonblocking: state.nonblocking,
        }
    }

    #[must_use]
    pub const fn status(&self) -> SignalFdStatus {
        SignalFdStatus {
            mode: SIGNALFD_MODE,
            size: 0,
            link_count: 1,
        }
    }

    fn dequeue_first(&self) -> Result<SignalInfo, SignalFdError> {
        loop {
            let mask = self.current_mask()?;
            if let Some(info) = self.inner.queue.dequeue(mask)? {
                return Ok(info);
            }
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return Err(SignalFdError::Retired);
            }
            if state.nonblocking {
                return Err(SignalFdError::WouldBlock);
            }
            if self.inner.queue.has_pending(state.mask) {
                continue;
            }
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return Err(SignalFdError::Retired);
            }
        }
    }

    fn current_mask(&self) -> Result<SignalMask, SignalFdError> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            Err(SignalFdError::Retired)
        } else {
            Ok(state.mask)
        }
    }

    fn write_record(output: &mut [u8], index: usize, info: SignalInfo) {
        let start = index * SIGNALFD_RECORD_SIZE;
        output[start..start + SIGNALFD_RECORD_SIZE].copy_from_slice(&info.encode());
    }

    fn retire_inner(&self) {
        let subscription = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return;
            }
            state.retired = true;
            self.inner.changed.notify_all();
            state.subscription.take()
        };
        if let Some(subscription) = subscription {
            subscription.quiesce();
        }
        self.inner.readiness.notify();
        self.inner.readiness.close();
    }
}

impl OpenFileDescription for SignalFd {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Event
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        Ok(self.status().metadata())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        SignalFd::read(self, output).map_err(SignalFdError::object_error)
    }

    fn prepare_atomic_read(
        &self,
        maximum: usize,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        prepared::AtomicRead::prepare(self, maximum)
    }

    fn prepare_atomic_context(
        &self,
        maximum: usize,
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        match context.actor {
            Some(actor) => prepared::AtomicRead::prepare_context(self, maximum, actor),
            None => prepared::AtomicRead::prepare(self, maximum),
        }
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0)
            .map_err(SignalFdError::object_error)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        SignalFd::readiness(self, interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.readiness.subscribe(observer)
    }

    fn retire(&self) {
        self.retire_inner();
    }

    fn close(&self) {
        self.retire_inner();
    }
}
