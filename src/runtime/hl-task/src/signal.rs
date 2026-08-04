use std::array;
use std::collections::VecDeque;

use crate::{ProcessId, ThreadId};

pub const SIGNAL_COUNT: usize = 64;
pub const SIGNAL_FRAME_MAXIMUM: usize = 32;
const STANDARD_SIGNAL_MAX: u8 = 31;

/// Valid Linux signal number in the guest ABI.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SignalNumber(u8);

impl SignalNumber {
    pub const KILL: Self = Self(9);
    pub const CONTINUE: Self = Self(18);
    pub const STOP: Self = Self(19);

    pub const fn new(number: u8) -> Result<Self, SignalQueueError> {
        if number >= 1 && number <= SIGNAL_COUNT as u8 {
            Ok(Self(number))
        } else {
            Err(SignalQueueError::InvalidSignal)
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_realtime(self) -> bool {
        self.0 > STANDARD_SIGNAL_MAX
    }

    pub(crate) const fn default_action(self) -> DeliveryAction {
        match self.0 {
            17 | 23 | 28 => DeliveryAction::Ignore,
            18 => DeliveryAction::Continue,
            19..=22 => DeliveryAction::Stop,
            3..=8 | 11 | 24 | 25 | 31 => DeliveryAction::Terminate { dumped_core: true },
            _ => DeliveryAction::Terminate { dumped_core: false },
        }
    }

    const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SignalMask(u64);

impl SignalMask {
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        let unblockable = (1_u64 << (SignalNumber::KILL.0 - 1)) | (1_u64 << (SignalNumber::STOP.0 - 1));
        Self(bits & !unblockable)
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, signal: SignalNumber) -> bool {
        self.0 & (1_u64 << (signal.0 - 1)) != 0
    }

    #[must_use]
    pub const fn with(self, signal: SignalNumber) -> Self {
        Self::from_bits(self.0 | (1_u64 << (signal.0 - 1)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDisposition {
    Default,
    Ignore,
    Handler(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalAction {
    pub disposition: SignalDisposition,
    pub flags: u64,
    pub restorer: u64,
    pub mask: SignalMask,
}

impl SignalAction {
    pub const DEFAULT: Self = Self {
        disposition: SignalDisposition::Default,
        flags: 0,
        restorer: 0,
        mask: SignalMask::from_bits(0),
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlternateStack {
    Disabled,
    Enabled { pointer: u64, size: u64 },
    Autodisarm { pointer: u64, size: u64 },
    Active { pointer: u64, size: u64 },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SignalInfo {
    pub signal: SignalNumber,
    pub code: i32,
    pub error: i32,
    pub sender_process: u32,
    pub sender_user: u32,
    pub value: u64,
    pub address: u64,
    pub source_tag: u32,
}

impl SignalInfo {
    #[must_use]
    pub const fn bare(signal: SignalNumber) -> Self {
        Self {
            signal,
            code: 0,
            error: 0,
            sender_process: 0,
            sender_user: 0,
            value: 0,
            address: 0,
            source_tag: 0,
        }
    }

    #[must_use]
    pub const fn is_synchronous(self) -> bool {
        self.code > 0 && matches!(self.signal.get(), 4 | 5 | 7 | 8 | 11)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingTarget {
    Process(ProcessId),
    Thread(ThreadId),
}

#[derive(Debug)]
pub struct PreparedSignalWait {
    pub(crate) thread: ThreadId,
    pub(crate) process: ProcessId,
    pub(crate) info: SignalInfo,
    pub(crate) from_thread: bool,
    pub(crate) _reservation: SignalReservation,
}

pub struct PreparedForcedDelivery {
    pub(crate) thread: ThreadId,
    pub(crate) prepared: Option<PreparedSignalWait>,
    pub(crate) forced: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<ThreadId, PreparedSignalWait>>>,
}

impl PreparedForcedDelivery {
    #[must_use]
    pub fn info(&self) -> SignalInfo {
        self.prepared.as_ref().expect("prepared forced signal").info()
    }
}

impl Drop for PreparedForcedDelivery {
    fn drop(&mut self) {
        let Some(prepared) = self.prepared.take() else {
            return;
        };
        self.forced
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(self.thread, prepared);
    }
}

impl PreparedSignalWait {
    #[must_use]
    pub const fn info(&self) -> SignalInfo {
        self.info
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SignalReservationKey {
    pub(crate) thread: ThreadId,
    pub(crate) process: ProcessId,
    pub(crate) info: SignalInfo,
    pub(crate) from_thread: bool,
}

#[derive(Debug)]
pub(crate) struct SignalReservation {
    pub(crate) key: SignalReservationKey,
    pub(crate) reservations: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<SignalReservationKey>>>,
}

impl Drop for SignalReservation {
    fn drop(&mut self) {
        self.reservations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryAction {
    Ignore,
    Handle(SignalAction),
    Stop,
    Continue,
    Terminate { dumped_core: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalQueueError {
    InvalidSignal,
    QueueFull,
    InvalidAction,
}

#[derive(Clone)]
pub(crate) struct PendingSignals {
    queues: [VecDeque<SignalInfo>; SIGNAL_COUNT],
}

impl PendingSignals {
    pub(crate) fn new() -> Self {
        Self {
            queues: array::from_fn(|_| VecDeque::new()),
        }
    }

    pub(crate) fn enqueue(&mut self, info: SignalInfo, limit: usize) -> Result<bool, SignalQueueError> {
        let queue = &mut self.queues[info.signal.index()];
        if !info.signal.is_realtime() && !queue.is_empty() {
            return Ok(false);
        }
        if queue.len() >= limit {
            return Err(SignalQueueError::QueueFull);
        }
        queue.push_back(info);
        Ok(true)
    }

    pub(crate) fn enqueue_unique_source(&mut self, info: SignalInfo, limit: usize) -> Result<bool, SignalQueueError> {
        let queue = &self.queues[info.signal.index()];
        if info.source_tag != 0 && queue.iter().any(|queued| queued.source_tag == info.source_tag) {
            return Ok(false);
        }
        self.enqueue(info, limit)
    }

    pub(crate) fn remove_source(&mut self, signal: SignalNumber, source_tag: u32) -> bool {
        let queue = &mut self.queues[signal.index()];
        let before = queue.len();
        queue.retain(|info| info.source_tag != source_tag);
        queue.len() != before
    }

    pub(crate) fn peek_eligible(&self, blocked: SignalMask) -> Option<SignalNumber> {
        self.queues
            .iter()
            .enumerate()
            .rev()
            .find(|(index, queue)| !queue.is_empty() && !blocked.contains(SignalNumber((*index + 1) as u8)))
            .map(|(index, _)| SignalNumber((index + 1) as u8))
    }

    pub(crate) fn peek_synchronous(&self) -> Option<SignalNumber> {
        self.queues.iter().enumerate().rev().find_map(|(index, queue)| {
            queue.front().copied().filter(|info| info.is_synchronous())?;
            Some(SignalNumber((index + 1) as u8))
        })
    }

    pub(crate) fn peek_selected(&self, selected: SignalMask) -> Option<SignalNumber> {
        self.queues
            .iter()
            .enumerate()
            .find(|(index, queue)| !queue.is_empty() && selected.contains(SignalNumber((*index + 1) as u8)))
            .map(|(index, _)| SignalNumber((index + 1) as u8))
    }

    pub(crate) fn pop(&mut self, signal: SignalNumber) -> Option<SignalInfo> {
        let info = self.queues[signal.index()].pop_front()?;
        Some(info)
    }

    pub(crate) fn front(&self, signal: SignalNumber) -> Option<SignalInfo> {
        self.queues[signal.index()].front().copied()
    }

    pub(crate) fn flush(&mut self, signal: SignalNumber) {
        self.queues[signal.index()].clear();
    }

    pub(crate) fn snapshot(&self) -> Vec<SignalInfo> {
        self.queues.iter().flat_map(|queue| queue.iter().copied()).collect()
    }

    pub(crate) fn restore(values: &[SignalInfo], limit: usize) -> Result<Self, SignalQueueError> {
        let mut pending = Self::new();
        for value in values {
            if !pending.enqueue(*value, limit)? {
                return Err(SignalQueueError::InvalidAction);
            }
        }
        Ok(pending)
    }
}

#[derive(Clone)]
pub(crate) struct SignalProcessState {
    pub(crate) actions: [SignalAction; SIGNAL_COUNT],
    pub(crate) pending: PendingSignals,
}

impl SignalProcessState {
    pub(crate) fn new() -> Self {
        Self {
            actions: [SignalAction::DEFAULT; SIGNAL_COUNT],
            pending: PendingSignals::new(),
        }
    }

    pub(crate) fn fork_copy(&self) -> Self {
        Self {
            actions: self.actions,
            pending: PendingSignals::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SignalThreadState {
    pub(crate) mask: SignalMask,
    pub(crate) alternate_stack: AlternateStack,
    pub(crate) pending: PendingSignals,
    /// Signals that were already pending when the active handler frames were
    /// entered. Linux drains such a batch serially; only signals raised after
    /// the innermost entry may nest that handler.
    pub(crate) deferred: SignalMask,
    pub(crate) frames: Vec<SignalFrameScope>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalFrameScope {
    pub deferred: SignalMask,
    pub stack_pointer: u64,
}

impl SignalThreadState {
    pub(crate) fn new() -> Self {
        Self {
            mask: SignalMask::from_bits(0),
            alternate_stack: AlternateStack::Disabled,
            pending: PendingSignals::new(),
            deferred: SignalMask::from_bits(0),
            frames: Vec::new(),
        }
    }

    pub(crate) fn fork_copy(&self) -> Self {
        Self {
            mask: self.mask,
            alternate_stack: self.alternate_stack,
            pending: PendingSignals::new(),
            deferred: self.deferred,
            frames: self.frames.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalProcessSnapshot {
    pub actions: Vec<(SignalNumber, SignalAction)>,
    pub pending: Vec<SignalInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalThreadSnapshot {
    pub mask: SignalMask,
    pub alternate_stack: AlternateStack,
    pub pending: Vec<SignalInfo>,
    pub deferred: SignalMask,
    pub frames: Vec<SignalFrameScope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalForkPlan {
    pub process: SignalProcessSnapshot,
    pub thread: SignalThreadSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalExecPlan {
    pub process: SignalProcessSnapshot,
    pub thread: SignalThreadSnapshot,
}
