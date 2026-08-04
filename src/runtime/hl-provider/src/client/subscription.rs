//! Bounded unsolicited-event subscription values and state.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use crate::ProviderError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubscriptionIdentity {
    pub owner: u64,
    pub generation: u32,
}

impl SubscriptionIdentity {
    pub const fn new(owner: u64, generation: u32) -> Result<Self, ProviderError> {
        if owner == 0 || generation == 0 {
            return Err(ProviderError::InvalidSubscription);
        }
        Ok(Self { owner, generation })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubscriptionKey {
    id: NonZeroU64,
    generation: u32,
}

impl SubscriptionKey {
    pub(crate) fn new(id: u64, generation: u32) -> Self {
        Self {
            id: NonZeroU64::new(id).unwrap(),
            generation,
        }
    }

    #[must_use]
    pub const fn id(self) -> u64 {
        self.id.get()
    }

    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderEvent {
    pub subscription: SubscriptionKey,
    pub payload: Vec<u8>,
    pub lost: u64,
}

pub trait EventObserver: Send + Sync + 'static {
    fn provider_event(&self, event: ProviderEvent);
}

impl fmt::Debug for dyn EventObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EventObserver")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionSnapshot {
    pub identity: SubscriptionIdentity,
    pub key: SubscriptionKey,
    pub queued: usize,
    pub lost: u64,
}

#[derive(Debug)]
pub(crate) struct Subscription {
    pub identity: SubscriptionIdentity,
    pub key: SubscriptionKey,
    pub active: bool,
    pub observer: Arc<dyn EventObserver>,
    pub events: VecDeque<Vec<u8>>,
    pub lost: u64,
    pub callbacks: usize,
}

#[derive(Debug)]
pub(crate) struct SubscriptionSlot {
    pub generation: u32,
    pub subscription: Option<Subscription>,
}
