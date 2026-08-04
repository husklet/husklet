//! Public provider client values and bounded configuration.

use std::fmt;
use std::num::NonZeroU64;

use crate::TransportError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub(crate) NonZeroU64);

impl RequestId {
    pub(crate) fn new(value: u64) -> Self {
        Self(NonZeroU64::new(value).unwrap())
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ticket {
    pub(crate) slot: u32,
    pub(crate) generation: u32,
    pub(crate) request: RequestId,
}

impl Ticket {
    #[must_use]
    pub const fn request(self) -> RequestId {
        self.request
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub request: RequestId,
    pub payload: Vec<u8>,
    pub linux_errno: i32,
}

impl Reply {
    pub(crate) fn new(request: RequestId, payload: Vec<u8>) -> Self {
        let linux_errno = if payload.len() >= 7 && payload[0] == 0xff {
            i32::from_le_bytes(payload[1..5].try_into().unwrap())
        } else {
            0
        };
        Self {
            request,
            payload,
            linux_errno,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub payload_bytes: usize,
    pub in_flight: usize,
    pub subscriptions: usize,
    pub events_per_subscription: usize,
}
pub type ClientLimits = Limits;

impl ClientLimits {
    pub const fn new(payload_bytes: usize, in_flight: usize) -> Result<Self, ProviderError> {
        Self::with_subscriptions(payload_bytes, in_flight, in_flight, 16)
    }

    pub const fn with_subscriptions(
        payload_bytes: usize,
        in_flight: usize,
        subscriptions: usize,
        events_per_subscription: usize,
    ) -> Result<Self, ProviderError> {
        if payload_bytes == 0
            || payload_bytes > u32::MAX as usize
            || in_flight == 0
            || in_flight > u32::MAX as usize
            || subscriptions == 0
            || subscriptions > u32::MAX as usize
            || events_per_subscription == 0
            || events_per_subscription > u32::MAX as usize
        {
            return Err(ProviderError::InvalidLimits);
        }
        Ok(Self {
            payload_bytes,
            in_flight,
            subscriptions,
            events_per_subscription,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    InvalidLimits,
    PayloadTooLarge,
    Capacity,
    Closed,
    Canceled,
    InvalidTicket,
    InvalidSubscription,
    DuplicateSubscription,
    AlreadyComplete,
    CheckpointBusy,
    MalformedFrame,
    UnsupportedVersion,
    UnknownFrame(u16),
    UnexpectedFrame,
    ZeroProgress,
    Transport(TransportError),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider client {self:?}")
    }
}

impl std::error::Error for ProviderError {}

impl From<TransportError> for ProviderError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}
