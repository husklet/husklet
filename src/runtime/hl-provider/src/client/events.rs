//! Event queueing, dispatch, and quiescent subscription teardown.

use std::sync::Arc;

use super::{ClientCore, ProviderSubscription, SubscriptionControl};
use crate::protocol::FrameKind;
use crate::{EventObserver, ProviderError, ProviderEvent, ProviderTransport, SubscriptionIdentity, SubscriptionKey};

impl<T: ProviderTransport> ClientCore<T> {
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        identity: SubscriptionIdentity,
        payload: &[u8],
        observer: Arc<dyn EventObserver>,
    ) -> Result<ProviderSubscription, ProviderError> {
        let _admission = self.activity.admit();
        if payload.is_empty() || payload.len() > self.limits.payload_bytes {
            return Err(ProviderError::PayloadTooLarge);
        }
        let key = self.lock().reserve_subscription(identity, observer)?;
        if let Err(error) = self.send(FrameKind::Subscribe, key.id(), payload) {
            return match self.unsubscribe(key) {
                Ok(()) | Err(_) => Err(error),
            };
        }
        let control: Arc<dyn SubscriptionControl> = self.clone();
        Ok(ProviderSubscription::new(control, key))
    }

    pub(crate) fn deliver_event(&self, id: u64, payload: Vec<u8>) {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        let subscription = state.subscriptions.iter_mut().find_map(|slot| {
            slot.subscription
                .as_mut()
                .filter(|value| value.active && value.key.id() == id)
        });
        let Some(subscription) = subscription else {
            state.stale_events = state.stale_events.saturating_add(1);
            return;
        };
        if subscription.events.len() == self.limits.events_per_subscription {
            subscription.lost = subscription.lost.saturating_add(1);
            if let Some(latest) = subscription.events.back_mut() {
                *latest = payload;
            }
        } else {
            subscription.events.push_back(payload);
        }
        self.changed.notify_all();
    }

    pub(crate) fn dispatch_events(&self) {
        while let Some((observer, event)) = self.wait_for_event() {
            let _admission = self.activity.admit();
            observer.provider_event(event.clone());
            self.finish_callback(event.subscription);
        }
    }

    pub(crate) fn unsubscribe(&self, key: SubscriptionKey) -> Result<(), ProviderError> {
        self.deactivate(key)?;
        let send_result = self.send(FrameKind::Unsubscribe, key.id(), &[]);
        self.wait_for_callbacks(key)?;
        self.remove_subscription(key);
        send_result
    }

    fn wait_for_event(&self) -> Option<(Arc<dyn EventObserver>, ProviderEvent)> {
        let mut state = self.lock();
        loop {
            if let Some(delivery) = Self::next_event(&mut state) {
                return Some(delivery);
            }
            if state.stopping {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn next_event(state: &mut super::state::ClientState) -> Option<(Arc<dyn EventObserver>, ProviderEvent)> {
        state.subscriptions.iter_mut().find_map(|slot| {
            let subscription = slot.subscription.as_mut()?;
            if !subscription.active {
                return None;
            }
            let payload = subscription.events.pop_front()?;
            subscription.callbacks += 1;
            let lost = std::mem::take(&mut subscription.lost);
            Some((
                Arc::clone(&subscription.observer),
                ProviderEvent {
                    subscription: subscription.key,
                    payload,
                    lost,
                },
            ))
        })
    }

    fn finish_callback(&self, key: SubscriptionKey) {
        let mut state = self.lock();
        let Ok(subscription) = state.subscription_mut(key) else {
            return;
        };
        subscription.callbacks -= 1;
        self.changed.notify_all();
    }

    fn deactivate(&self, key: SubscriptionKey) -> Result<(), ProviderError> {
        let mut state = self.lock();
        let subscription = state.subscription_mut(key)?;
        subscription.active = false;
        subscription.events.clear();
        self.changed.notify_all();
        Ok(())
    }

    fn wait_for_callbacks(&self, key: SubscriptionKey) -> Result<(), ProviderError> {
        let mut state = self.lock();
        while state.subscription_mut(key)?.callbacks != 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        Ok(())
    }

    fn remove_subscription(&self, key: SubscriptionKey) {
        self.lock().remove_subscription(key);
    }
}

impl<T: ProviderTransport> SubscriptionControl for ClientCore<T> {
    fn unsubscribe(&self, key: SubscriptionKey) -> Result<(), ProviderError> {
        self.unsubscribe(key)
    }
}
