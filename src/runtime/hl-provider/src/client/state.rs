//! Shared request and subscription state owned by one provider connection.

use super::subscription::{Subscription, SubscriptionSlot};
use crate::{ProviderError, Reply, RequestId, SubscriptionSnapshot, Ticket};

#[derive(Debug)]
pub(crate) struct Waiter {
    pub(crate) generation: u32,
    pub(crate) request: RequestId,
    pub(crate) completion: Option<Result<Reply, ProviderError>>,
}

#[derive(Debug)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) waiter: Option<Waiter>,
}

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) slots: Vec<Slot>,
    pub(crate) subscriptions: Vec<SubscriptionSlot>,
    next_request: u64,
    next_subscription: u64,
    pub(crate) peer_error: Option<ProviderError>,
    pub(crate) stopping: bool,
    pub(crate) late_replies: u64,
    pub(crate) stale_events: u64,
}
pub(crate) type ClientState = State;

impl ClientState {
    pub(crate) fn checkpoint(&self) -> crate::ProviderClientCheckpoint {
        crate::ProviderClientCheckpoint {
            request_generations: self.slots.iter().map(|slot| slot.generation).collect(),
            subscription_generations: self.subscriptions.iter().map(|slot| slot.generation).collect(),
            next_request: self.next_request,
            next_subscription: self.next_subscription,
            late_replies: self.late_replies,
            stale_events: self.stale_events,
            subscriptions: self
                .subscriptions
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    slot.subscription
                        .as_ref()
                        .map(|subscription| crate::ProviderSubscriptionCheckpoint {
                            slot: index,
                            identity_owner: subscription.identity.owner,
                            identity_generation: subscription.identity.generation,
                            key_id: subscription.key.id(),
                            key_generation: subscription.key.generation(),
                            queued: subscription.events.iter().cloned().collect(),
                            lost: subscription.lost,
                        })
                })
                .collect(),
        }
    }

    pub(crate) fn new(in_flight: usize, subscriptions: usize) -> Self {
        Self {
            slots: (0..in_flight)
                .map(|_| Slot {
                    generation: 0,
                    waiter: None,
                })
                .collect(),
            subscriptions: (0..subscriptions)
                .map(|_| SubscriptionSlot {
                    generation: 0,
                    subscription: None,
                })
                .collect(),
            next_request: 1,
            next_subscription: 1,
            peer_error: None,
            stopping: false,
            late_replies: 0,
            stale_events: 0,
        }
    }

    pub(crate) fn available(&self) -> Result<(), ProviderError> {
        if self.stopping {
            return Err(ProviderError::Closed);
        }
        if let Some(error) = &self.peer_error {
            return Err(error.clone());
        }
        Ok(())
    }

    pub(crate) fn allocate_request(&mut self) -> Result<u64, ProviderError> {
        for _ in 0..=self.slots.len() {
            let candidate = self.next_request;
            self.next_request = self.next_request.wrapping_add(1).max(1);
            if candidate != 0 && !self.contains_request(candidate) {
                return Ok(candidate);
            }
        }
        Err(ProviderError::Capacity)
    }

    pub(crate) fn allocate_subscription(&mut self) -> u64 {
        let id = self.next_subscription;
        self.next_subscription = self.next_subscription.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn subscription_snapshots(&self) -> Vec<SubscriptionSnapshot> {
        self.subscriptions
            .iter()
            .filter_map(|slot| {
                slot.subscription.as_ref().map(|value| SubscriptionSnapshot {
                    identity: value.identity,
                    key: value.key,
                    queued: value.events.len(),
                    lost: value.lost,
                })
            })
            .collect()
    }

    pub(crate) fn waiter(&self, ticket: Ticket) -> Result<&Waiter, ProviderError> {
        let slot = self
            .slots
            .get(ticket.slot as usize)
            .ok_or(ProviderError::InvalidTicket(ticket.request.get()))?;
        let waiter = slot
            .waiter
            .as_ref()
            .ok_or(ProviderError::InvalidTicket(ticket.request.get()))?;
        if waiter.generation != ticket.generation || waiter.request != ticket.request {
            return Err(ProviderError::InvalidTicket(ticket.request.get()));
        }
        Ok(waiter)
    }

    pub(crate) fn waiter_mut(&mut self, ticket: Ticket) -> Result<&mut Waiter, ProviderError> {
        let slot = self
            .slots
            .get_mut(ticket.slot as usize)
            .ok_or(ProviderError::InvalidTicket(ticket.request.get()))?;
        let waiter = slot
            .waiter
            .as_mut()
            .ok_or(ProviderError::InvalidTicket(ticket.request.get()))?;
        if waiter.generation != ticket.generation || waiter.request != ticket.request {
            return Err(ProviderError::InvalidTicket(ticket.request.get()));
        }
        Ok(waiter)
    }

    pub(crate) fn reserve_subscription(
        &mut self,
        identity: crate::SubscriptionIdentity,
        observer: std::sync::Arc<dyn crate::EventObserver>,
    ) -> Result<crate::SubscriptionKey, ProviderError> {
        self.available()?;
        if self.has_subscription(identity) {
            return Err(ProviderError::DuplicateSubscription);
        }
        let index = self
            .subscriptions
            .iter()
            .position(|slot| slot.subscription.is_none())
            .ok_or(ProviderError::Capacity)?;
        let id = self.allocate_subscription();
        let slot = &mut self.subscriptions[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        let key = crate::SubscriptionKey::new(id, slot.generation);
        slot.subscription = Some(Subscription {
            identity,
            key,
            active: true,
            observer,
            events: Default::default(),
            lost: 0,
            callbacks: 0,
        });
        Ok(key)
    }

    pub(crate) fn subscription_mut(&mut self, key: crate::SubscriptionKey) -> Result<&mut Subscription, ProviderError> {
        self.subscriptions
            .iter_mut()
            .find_map(|slot| slot.subscription.as_mut().filter(|value| value.key == key))
            .ok_or(ProviderError::InvalidSubscription)
    }

    pub(crate) fn remove_subscription(&mut self, key: crate::SubscriptionKey) {
        let Some(slot) = self
            .subscriptions
            .iter_mut()
            .find(|slot| slot.subscription.as_ref().is_some_and(|value| value.key == key))
        else {
            return;
        };
        slot.subscription = None;
    }

    fn contains_request(&self, candidate: u64) -> bool {
        self.slots.iter().any(|slot| {
            slot.waiter
                .as_ref()
                .is_some_and(|waiter| waiter.request.get() == candidate)
        })
    }

    fn has_subscription(&self, identity: crate::SubscriptionIdentity) -> bool {
        self.subscriptions.iter().any(|slot| {
            slot.subscription
                .as_ref()
                .is_some_and(|value| value.identity == identity)
        })
    }
}
