use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use hl_provider::{
    NamespaceError, PROVIDER_CHECKPOINT_EVENT_BYTE_MAXIMUM, PROVIDER_CHECKPOINT_FILE_MAXIMUM,
    PROVIDER_CHECKPOINT_PATH_BYTE_MAXIMUM, ProviderCheckpointCapture, ProviderCheckpointImage,
    ProviderCheckpointReconnect, ProviderClientCheckpoint, ProviderFileCheckpoint, ProviderRemoteRestore,
    ProviderResourceKey, RemoteId,
};

use super::transaction::RemoteTransaction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Invalid,
    Missing,
    Exhausted,
    Stale,
}

struct ActivityState {
    frozen: bool,
    admitted: usize,
}

pub(super) struct Activity {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

impl Activity {
    fn admit(self: &Arc<Self>) -> Admission {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        while state.frozen {
            state = self.changed.wait(state).unwrap_or_else(|error| error.into_inner());
        }
        state.admitted += 1;
        Admission(Arc::clone(self))
    }

    fn freeze(&self) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::Invalid)?;
        if state.frozen {
            return Err(Error::Invalid);
        }
        state.frozen = true;
        while state.admitted != 0 {
            state = self.changed.wait(state).map_err(|_| Error::Invalid)?;
        }
        Ok(())
    }

    pub(super) fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.frozen = false;
        self.changed.notify_all();
    }

    fn frozen(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).frozen
    }
}

struct Admission(Arc<Activity>);

impl Drop for Admission {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.admitted -= 1;
        if state.admitted == 0 {
            self.0.changed.notify_all();
        }
    }
}

pub(super) struct Resource {
    key: ProviderResourceKey,
    pub(super) remote: RemoteId,
    owners: Mutex<u64>,
}

#[derive(Clone)]
pub(super) struct Publication {
    pub(super) by_remote: BTreeMap<u64, Arc<Resource>>,
    pub(super) by_key: BTreeMap<ProviderResourceKey, Arc<Resource>>,
    files: Vec<ProviderFileCheckpoint>,
    client: ProviderClientCheckpoint,
}

pub(super) struct State {
    pub(super) generation: u64,
    pub(super) current: Arc<Publication>,
}

pub(super) struct Store {
    pub(super) activity: Arc<Activity>,
    pub(super) state: Mutex<State>,
    next_key: AtomicU64,
}

#[derive(Clone)]
pub struct Registry {
    store: Arc<Store>,
}

pub struct Lease {
    registry: Registry,
    resource: Arc<Resource>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        let client = ProviderClientCheckpoint {
            request_generations: Vec::new(),
            subscription_generations: Vec::new(),
            next_request: 1,
            next_subscription: 1,
            late_replies: 0,
            stale_events: 0,
            subscriptions: Vec::new(),
        };
        Self {
            store: Arc::new(Store {
                activity: Arc::new(Activity {
                    state: Mutex::new(ActivityState {
                        frozen: false,
                        admitted: 0,
                    }),
                    changed: Condvar::new(),
                }),
                state: Mutex::new(State {
                    generation: 1,
                    current: Arc::new(Publication {
                        by_remote: BTreeMap::new(),
                        by_key: BTreeMap::new(),
                        files: Vec::new(),
                        client,
                    }),
                }),
                next_key: AtomicU64::new(1),
            }),
        }
    }

    pub fn register(&self, remote: RemoteId) -> Result<Lease, Error> {
        let _admission = self.store.activity.admit();
        let mut state = self.store.state.lock().map_err(|_| Error::Invalid)?;
        if let Some(resource) = state.current.by_remote.get(&remote.get()).cloned() {
            let mut owners = resource.owners.lock().map_err(|_| Error::Invalid)?;
            *owners = owners.checked_add(1).ok_or(Error::Exhausted)?;
            drop(owners);
            return Ok(Lease {
                registry: self.clone(),
                resource,
            });
        }
        let value = self
            .store
            .next_key
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
            .map_err(|_| Error::Exhausted)?;
        let key = ProviderResourceKey::new(value).ok_or(Error::Exhausted)?;
        let resource = Arc::new(Resource {
            key,
            remote,
            owners: Mutex::new(1),
        });
        let mut current = (*state.current).clone();
        current.by_remote.insert(remote.get(), Arc::clone(&resource));
        current.by_key.insert(key, Arc::clone(&resource));
        let generation = state.generation.checked_add(1).ok_or(Error::Exhausted)?;
        state.current = Arc::new(current);
        state.generation = generation;
        Ok(Lease {
            registry: self.clone(),
            resource,
        })
    }

    pub fn replace_projected(
        &self,
        files: Vec<ProviderFileCheckpoint>,
        client: ProviderClientCheckpoint,
    ) -> Result<(), Error> {
        let _admission = self.store.activity.admit();
        let mut state = self.store.state.lock().map_err(|_| Error::Invalid)?;
        Self::validate_projected(&state.current, &files, &client)?;
        let mut current = (*state.current).clone();
        current.files = files;
        current.client = client;
        let generation = state.generation.checked_add(1).ok_or(Error::Exhausted)?;
        state.current = Arc::new(current);
        state.generation = generation;
        Ok(())
    }

    #[must_use]
    pub fn projected(&self) -> (Vec<ProviderFileCheckpoint>, ProviderClientCheckpoint) {
        let state = self.store.state.lock().unwrap_or_else(|error| error.into_inner());
        (state.current.files.clone(), state.current.client.clone())
    }

    fn validate_projected(
        current: &Publication,
        files: &[ProviderFileCheckpoint],
        client: &ProviderClientCheckpoint,
    ) -> Result<(), Error> {
        Self::validate_files(current, files)?;
        Self::validate_client(client)
    }

    fn validate_files(current: &Publication, files: &[ProviderFileCheckpoint]) -> Result<(), Error> {
        if files.len() > PROVIDER_CHECKPOINT_FILE_MAXIMUM {
            return Err(Error::Invalid);
        }
        let mut descriptors = BTreeSet::new();
        let mut previous = None;
        for file in files {
            let resource = current.by_key.get(&file.resource).ok_or(Error::Missing)?;
            if resource.remote != file.snapshot.remote
                || file.snapshot.path.len() > PROVIDER_CHECKPOINT_PATH_BYTE_MAXIMUM
                || !descriptors.insert(file.descriptor)
                || !Self::key_follows(previous, file.descriptor)
            {
                return Err(Error::Invalid);
            }
            previous = Some(file.descriptor);
        }
        Ok(())
    }

    fn validate_client(client: &ProviderClientCheckpoint) -> Result<(), Error> {
        if client.next_request == 0
            || client.next_subscription == 0
            || client.subscriptions.len() > client.subscription_generations.len()
        {
            return Err(Error::Invalid);
        }
        let mut slots = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut event_bytes = 0_usize;
        let mut previous = None;
        for subscription in &client.subscriptions {
            if subscription.slot >= client.subscription_generations.len()
                || client.subscription_generations[subscription.slot] != subscription.key_generation
                || !slots.insert(subscription.slot)
                || !keys.insert((subscription.key_id, subscription.key_generation))
                || subscription.identity_owner == 0
                || subscription.identity_generation == 0
                || subscription.key_id == 0
                || subscription.key_generation == 0
                || !Self::slot_follows(previous, subscription.slot)
            {
                return Err(Error::Invalid);
            }
            previous = Some(subscription.slot);
            for event in &subscription.queued {
                event_bytes = Self::event_total(event_bytes, event.len())?;
            }
        }
        Ok(())
    }

    fn event_total(current: usize, additional: usize) -> Result<usize, Error> {
        let total = current.checked_add(additional).ok_or(Error::Exhausted)?;
        if total > PROVIDER_CHECKPOINT_EVENT_BYTE_MAXIMUM {
            return Err(Error::Exhausted);
        }
        Ok(total)
    }

    fn slot_follows(previous: Option<usize>, current: usize) -> bool {
        match previous {
            Some(previous) => previous < current,
            None => true,
        }
    }

    fn key_follows(previous: Option<ProviderResourceKey>, current: ProviderResourceKey) -> bool {
        match previous {
            Some(previous) => previous < current,
            None => true,
        }
    }

    fn transaction(&self, image: &ProviderCheckpointImage) -> Result<RemoteTransaction, NamespaceError> {
        let state = self.store.state.lock().map_err(|_| NamespaceError::InvalidSnapshot)?;
        let mut by_remote = BTreeMap::new();
        let mut by_key = BTreeMap::new();
        for reference in &image.resources {
            let resource = state
                .current
                .by_key
                .get(&reference.key)
                .cloned()
                .ok_or(NamespaceError::InvalidSnapshot)?;
            let entry = image
                .namespace
                .entries
                .iter()
                .find(|entry| entry.slot == reference.slot)
                .ok_or(NamespaceError::InvalidSnapshot)?;
            if entry.remote != resource.remote {
                return Err(NamespaceError::InvalidSnapshot);
            }
            by_remote.insert(resource.remote.get(), Arc::clone(&resource));
            by_key.insert(resource.key, resource);
        }
        let replacement = Arc::new(Publication {
            by_remote,
            by_key,
            files: image.files.clone(),
            client: image.client.clone(),
        });
        Ok(RemoteTransaction::new(
            Arc::clone(&self.store),
            state.generation,
            Arc::clone(&state.current),
            replacement,
        ))
    }

    fn release(&self, resource: &Arc<Resource>) {
        let _admission = self.store.activity.admit();
        let mut state = self.store.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut owners = resource.owners.lock().unwrap_or_else(|error| error.into_inner());
        *owners -= 1;
        if *owners != 0 {
            return;
        }
        drop(owners);
        let exact = state
            .current
            .by_key
            .get(&resource.key)
            .is_some_and(|value| Arc::ptr_eq(value, resource));
        if !exact {
            return;
        }
        let mut current = (*state.current).clone();
        current.by_key.remove(&resource.key);
        current.by_remote.remove(&resource.remote.get());
        current.files.retain(|file| file.resource != resource.key);
        state.current = Arc::new(current);
        state.generation = state.generation.saturating_add(1);
    }

    fn clone_lease(&self, resource: &Arc<Resource>) -> Result<Lease, Error> {
        let _admission = self.store.activity.admit();
        let state = self.store.state.lock().map_err(|_| Error::Invalid)?;
        let exact = state
            .current
            .by_key
            .get(&resource.key)
            .is_some_and(|value| Arc::ptr_eq(value, resource));
        if !exact {
            return Err(Error::Stale);
        }
        let mut owners = resource.owners.lock().map_err(|_| Error::Invalid)?;
        *owners = owners.checked_add(1).ok_or(Error::Exhausted)?;
        drop(owners);
        Ok(Lease {
            registry: self.clone(),
            resource: Arc::clone(resource),
        })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Lease {
    #[must_use]
    pub fn key(&self) -> ProviderResourceKey {
        self.resource.key
    }

    #[must_use]
    pub fn remote(&self) -> RemoteId {
        self.resource.remote
    }

    pub fn try_clone(&self) -> Result<Self, Error> {
        self.registry.clone_lease(&self.resource)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.registry.release(&self.resource);
    }
}

impl ProviderCheckpointCapture for Registry {
    fn freeze(&self) -> Result<(), NamespaceError> {
        self.store
            .activity
            .freeze()
            .map_err(|_| NamespaceError::InvalidSnapshot)
    }

    fn thaw(&self) {
        self.store.activity.thaw();
    }

    fn resource_key(&self, _: usize, remote: RemoteId) -> Result<ProviderResourceKey, NamespaceError> {
        if !self.store.activity.frozen() {
            return Err(NamespaceError::InvalidSnapshot);
        }
        self.store
            .state
            .lock()
            .map_err(|_| NamespaceError::InvalidSnapshot)?
            .current
            .by_remote
            .get(&remote.get())
            .map(|resource| resource.key)
            .ok_or(NamespaceError::InvalidSnapshot)
    }

    fn projected_state(&self) -> Result<(Vec<ProviderFileCheckpoint>, ProviderClientCheckpoint), NamespaceError> {
        if !self.store.activity.frozen() {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(self.projected())
    }
}

impl ProviderCheckpointReconnect for Registry {
    fn stage(&self, image: &ProviderCheckpointImage) -> Result<Box<dyn ProviderRemoteRestore>, NamespaceError> {
        image.validate()?;
        self.store
            .activity
            .freeze()
            .map_err(|_| NamespaceError::InvalidSnapshot)?;
        match self.transaction(image) {
            Ok(transaction) => Ok(Box::new(transaction)),
            Err(error) => {
                self.store.activity.thaw();
                Err(error)
            }
        }
    }
}
