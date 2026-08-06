//! Concurrent provider client and sole-reader reply demultiplexer.

mod events;
pub(crate) mod model;
mod state;
pub(crate) mod subscription;

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use self::state::{ClientState, Waiter};
use crate::protocol::{FrameKind, HEADER_SIZE, Header};
use crate::{
    ClientLimits, EventObserver, ProviderError, ProviderTransport, Reply, RequestId, SubscriptionIdentity,
    SubscriptionKey, SubscriptionSnapshot, Ticket, TransportError,
};

pub(crate) struct ClientCore<T> {
    pub(crate) transport: Arc<T>,
    pub(crate) limits: ClientLimits,
    state: Mutex<ClientState>,
    pub(crate) changed: Condvar,
    write: Mutex<()>,
    pub(crate) activity: Arc<crate::checkpoint_activity::CheckpointActivity>,
}

enum IncomingFrame {
    Reply(u64, Vec<u8>),
    Event(u64, Vec<u8>),
}

pub struct Provider<T: ProviderTransport> {
    shared: Arc<ClientCore<T>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    dispatcher: Mutex<Option<JoinHandle<()>>>,
}

pub(crate) trait SubscriptionControl: Send + Sync {
    fn unsubscribe(&self, key: SubscriptionKey) -> Result<(), ProviderError>;
}

pub struct ProviderSubscription {
    control: Arc<dyn SubscriptionControl>,
    key: SubscriptionKey,
    active: bool,
}

impl ProviderSubscription {
    pub(crate) fn new(control: Arc<dyn SubscriptionControl>, key: SubscriptionKey) -> Self {
        Self {
            control,
            key,
            active: true,
        }
    }
    #[must_use]
    pub const fn key(&self) -> SubscriptionKey {
        self.key
    }

    pub fn unsubscribe(mut self) -> Result<(), ProviderError> {
        self.active = false;
        self.control.unsubscribe(self.key)
    }
}

impl Drop for ProviderSubscription {
    fn drop(&mut self) {
        if self.active {
            let _ = self.control.unsubscribe(self.key);
            self.active = false;
        }
    }
}

impl<T: ProviderTransport> Provider<T> {
    pub fn new(transport: T, limits: ClientLimits) -> Result<Self, ProviderError> {
        ClientLimits::with_subscriptions(
            limits.payload_bytes,
            limits.in_flight,
            limits.subscriptions,
            limits.events_per_subscription,
        )?;
        let shared = Arc::new(ClientCore {
            transport: Arc::new(transport),
            limits,
            state: Mutex::new(ClientState::new(limits.in_flight, limits.subscriptions)),
            changed: Condvar::new(),
            write: Mutex::new(()),
            activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
        });
        let reader_shared = Arc::clone(&shared);
        let dispatcher_shared = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("hl-provider-reader".into())
            .spawn(move || reader_shared.reader())
            .map_err(|_| ProviderError::Transport(TransportError::Failed))?;
        let dispatcher = thread::Builder::new()
            .name("hl-provider-events".into())
            .spawn(move || dispatcher_shared.dispatch_events())
            .map_err(|_| ProviderError::Transport(TransportError::Failed))?;
        Ok(Self {
            shared,
            reader: Mutex::new(Some(reader)),
            dispatcher: Mutex::new(Some(dispatcher)),
        })
    }

    pub fn begin(&self, payload: &[u8]) -> Result<Ticket, ProviderError> {
        let _admission = self.shared.activity.admit();
        if payload.len() > self.shared.limits.payload_bytes {
            return Err(ProviderError::PayloadTooLarge);
        }
        let ticket = {
            let mut state = self.shared.lock();
            state.available()?;
            let slot_index = state
                .slots
                .iter()
                .position(|slot| slot.waiter.is_none())
                .ok_or(ProviderError::Capacity)?;
            let request = state.allocate_request()?;
            let slot = &mut state.slots[slot_index];
            slot.generation = slot.generation.wrapping_add(1).max(1);
            let request = RequestId::new(request);
            let ticket = Ticket {
                slot: slot_index as u32,
                generation: slot.generation,
                request,
            };
            slot.waiter = Some(Waiter {
                generation: ticket.generation,
                request,
                completion: None,
            });
            ticket
        };
        if let Err(error) = self.shared.send(FrameKind::Request, ticket.request.get(), payload) {
            self.shared.discard(ticket);
            self.shared.stop(error.clone());
            self.shared.transport.shutdown();
            return Err(error);
        }
        Ok(ticket)
    }

    pub fn request(&self, payload: &[u8]) -> Result<Reply, ProviderError> {
        self.wait(self.begin(payload)?)
    }

    pub fn wait(&self, ticket: Ticket) -> Result<Reply, ProviderError> {
        let mut state = self.shared.lock();
        loop {
            let waiter = state.waiter(ticket)?;
            if waiter.completion.is_some() {
                break;
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let slot = &mut state.slots[ticket.slot as usize];
        let waiter = slot.waiter.take().ok_or(ProviderError::InvalidTicket)?;
        waiter.completion.ok_or(ProviderError::InvalidTicket)?
    }

    pub fn cancel(&self, ticket: Ticket) -> Result<(), ProviderError> {
        {
            let mut state = self.shared.lock();
            let waiter = state.waiter_mut(ticket)?;
            if waiter.completion.is_some() {
                return Err(ProviderError::AlreadyComplete);
            }
            waiter.completion = Some(Err(ProviderError::Canceled));
            self.shared.changed.notify_all();
        }
        let _ = self.shared.send(FrameKind::Cancel, ticket.request.get(), &[]);
        Ok(())
    }

    pub fn subscribe(
        &self,
        identity: SubscriptionIdentity,
        payload: &[u8],
        observer: Arc<dyn EventObserver>,
    ) -> Result<ProviderSubscription, ProviderError> {
        self.shared.subscribe(identity, payload, observer)
    }

    #[must_use]
    pub fn subscription_snapshots(&self) -> Vec<SubscriptionSnapshot> {
        self.shared.lock().subscription_snapshots()
    }

    #[must_use]
    pub fn stale_events(&self) -> u64 {
        self.shared.lock().stale_events
    }

    #[must_use]
    pub fn late_replies(&self) -> u64 {
        self.shared.lock().late_replies
    }

    pub fn close(&self) {
        self.shared.stop(ProviderError::Closed);
        self.shared.transport.shutdown();
        let reader = self
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        let dispatcher = self
            .dispatcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(dispatcher) = dispatcher {
            let _ = dispatcher.join();
        }
    }

    pub fn freeze_checkpoint(&self) -> Result<(), ProviderError> {
        self.shared.activity.freeze();
        let state = self.shared.lock();
        let busy = state.slots.iter().any(|slot| slot.waiter.is_some())
            || state
                .subscriptions
                .iter()
                .any(|slot| slot.subscription.as_ref().is_some_and(|value| value.callbacks != 0));
        drop(state);
        if busy {
            self.shared.activity.thaw();
            return Err(ProviderError::CheckpointBusy);
        }
        Ok(())
    }

    pub fn checkpoint_client(&self) -> Result<crate::ProviderClientCheckpoint, ProviderError> {
        if !self.shared.activity.frozen() {
            return Err(ProviderError::CheckpointBusy);
        }
        Ok(self.shared.lock().checkpoint())
    }

    pub fn thaw_checkpoint(&self) {
        self.shared.activity.thaw();
    }
}

impl<T: ProviderTransport> Drop for Provider<T> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<T: ProviderTransport> ClientCore<T> {
    pub(crate) fn lock(&self) -> MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn send(&self, kind: FrameKind, request: u64, payload: &[u8]) -> Result<(), ProviderError> {
        if payload.len() > self.limits.payload_bytes {
            return Err(ProviderError::PayloadTooLarge);
        }
        let header = Header::encode(kind, payload.len(), request)?;
        let _guard = self.write.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.write_exact(&header)?;
        self.write_exact(payload)
    }

    fn write_exact(&self, bytes: &[u8]) -> Result<(), ProviderError> {
        let mut offset = 0;
        while offset < bytes.len() {
            match self.transport.write(&bytes[offset..]) {
                Ok(0) => return Err(ProviderError::ZeroProgress),
                Ok(count) if count <= bytes.len() - offset => offset += count,
                Ok(_) => return Err(ProviderError::ZeroProgress),
                Err(TransportError::Interrupted) => {}
                Err(TransportError::WouldBlock) => self.transport.wait_writable()?,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn read_exact(&self, bytes: &mut [u8]) -> Result<(), ProviderError> {
        let mut offset = 0;
        while offset < bytes.len() {
            match self.transport.read(&mut bytes[offset..]) {
                Ok(0) => return Err(ProviderError::Closed),
                Ok(count) if count <= bytes.len() - offset => offset += count,
                Ok(_) => return Err(ProviderError::ZeroProgress),
                Err(TransportError::Interrupted) => {}
                Err(TransportError::WouldBlock) => self.transport.wait_readable()?,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn reader(&self) {
        loop {
            let result = self.read_frame();
            match result {
                Ok(IncomingFrame::Reply(request, payload)) => {
                    self.deliver_reply(request, payload);
                }
                Ok(IncomingFrame::Event(subscription, payload)) => {
                    self.deliver_event(subscription, payload);
                }
                Err(error) => {
                    self.stop(error);
                    return;
                }
            }
        }
    }

    fn read_frame(&self) -> Result<IncomingFrame, ProviderError> {
        let mut header_bytes = [0_u8; HEADER_SIZE];
        self.read_exact(&mut header_bytes)?;
        let header = Header::decode(&header_bytes, self.limits.payload_bytes)?;
        if header.request == 0 || !matches!(header.kind, FrameKind::Reply | FrameKind::Event) {
            return Err(ProviderError::UnexpectedFrame);
        }
        let mut payload = vec![0_u8; header.size];
        self.read_exact(&mut payload)?;
        match header.kind {
            FrameKind::Reply => Ok(IncomingFrame::Reply(header.request, payload)),
            FrameKind::Event => Ok(IncomingFrame::Event(header.request, payload)),
            _ => Err(ProviderError::UnexpectedFrame),
        }
    }

    fn deliver_reply(&self, request: u64, payload: Vec<u8>) {
        let _admission = self.activity.admit();
        let mut state = self.lock();
        let waiter = state.slots.iter_mut().find_map(|slot| {
            slot.waiter
                .as_mut()
                .filter(|waiter| waiter.request.get() == request && waiter.completion.is_none())
        });
        if let Some(waiter) = waiter {
            waiter.completion = Some(Ok(Reply::new(waiter.request, payload)));
            self.changed.notify_all();
        } else {
            state.late_replies = state.late_replies.saturating_add(1);
        }
    }

    fn discard(&self, ticket: Ticket) {
        let mut state = self.lock();
        if state.waiter(ticket).is_ok() {
            state.slots[ticket.slot as usize].waiter = None;
        }
    }

    fn stop(&self, error: ProviderError) {
        let mut state = self.lock();
        if state.stopping {
            return;
        }
        state.stopping = true;
        state.peer_error = Some(error.clone());
        for waiter in state.slots.iter_mut().filter_map(|slot| slot.waiter.as_mut()) {
            if waiter.completion.is_none() {
                waiter.completion = Some(Err(error.clone()));
            }
        }
        for subscription in state
            .subscriptions
            .iter_mut()
            .filter_map(|slot| slot.subscription.as_mut())
        {
            subscription.active = false;
            subscription.events.clear();
        }
        self.changed.notify_all();
    }
}
