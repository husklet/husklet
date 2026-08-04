use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use crate::namespace::{MAX_CAPACITY, RemoteLease, Resource, Slot, State};
use crate::{FileSnapshot, HandleNamespace, NamespaceError, NamespaceSnapshot, RemoteId};

pub const PROVIDER_CHECKPOINT_VERSION: u32 = 1;
pub const PROVIDER_CHECKPOINT_FILE_MAXIMUM: usize = 1 << 20;
pub const PROVIDER_CHECKPOINT_EVENT_BYTE_MAXIMUM: usize = 1 << 28;
pub const PROVIDER_CHECKPOINT_PATH_BYTE_MAXIMUM: usize = crate::file::PATH_MAXIMUM;

impl ProviderCheckpointImage {
    pub(crate) fn restore_namespace(
        snapshot: &NamespaceSnapshot,
        remotes: &[(usize, RemoteId)],
    ) -> Result<HandleNamespace, NamespaceError> {
        if snapshot.capacity == 0
            || snapshot.capacity > MAX_CAPACITY
            || snapshot.generations.len() != snapshot.capacity
            || snapshot.live != snapshot.entries.len()
            || remotes.len() != snapshot.entries.len()
        {
            return Err(NamespaceError::InvalidSnapshot);
        }
        let mut slots = snapshot
            .generations
            .iter()
            .copied()
            .map(|generation| Slot {
                generation,
                reserved: false,
                resource: None,
            })
            .collect::<Vec<_>>();
        let mut references = 0_u64;
        for entry in &snapshot.entries {
            let remote = Self::restore_remote(entry.slot, entry.generation, entry.references, &slots, remotes)?;
            references = references
                .checked_add(u64::from(entry.references))
                .ok_or(NamespaceError::InvalidSnapshot)?;
            slots[entry.slot].resource = Some(Resource {
                lease: Arc::new(RemoteLease::new(remote, entry.kind)),
                references: entry.references,
            });
        }
        if references != snapshot.references {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(HandleNamespace {
            state: Mutex::new(State { slots }),
            activity: Arc::new(crate::checkpoint_activity::CheckpointActivity::default()),
        })
    }

    fn restore_remote(
        slot: usize,
        generation: u16,
        references: u32,
        slots: &[Slot],
        remotes: &[(usize, RemoteId)],
    ) -> Result<RemoteId, NamespaceError> {
        if slot >= slots.len()
            || references == 0
            || generation != slots[slot].generation
            || slots[slot].resource.is_some()
        {
            return Err(NamespaceError::InvalidSnapshot);
        }
        let mut matches = remotes
            .iter()
            .filter_map(|(candidate, remote)| (*candidate == slot).then_some(*remote));
        let remote = matches.next().ok_or(NamespaceError::InvalidSnapshot)?;
        if matches.next().is_some() {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(remote)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProviderResourceKey(NonZeroU64);

impl ProviderResourceKey {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderResourceReference {
    pub slot: usize,
    pub key: ProviderResourceKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFileCheckpoint {
    pub descriptor: ProviderResourceKey,
    pub resource: ProviderResourceKey,
    pub snapshot: FileSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSubscriptionCheckpoint {
    pub slot: usize,
    pub identity_owner: u64,
    pub identity_generation: u32,
    pub key_id: u64,
    pub key_generation: u32,
    pub queued: Vec<Vec<u8>>,
    pub lost: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderClientCheckpoint {
    pub request_generations: Vec<u32>,
    pub subscription_generations: Vec<u32>,
    pub next_request: u64,
    pub next_subscription: u64,
    pub late_replies: u64,
    pub stale_events: u64,
    pub subscriptions: Vec<ProviderSubscriptionCheckpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCheckpointImage {
    pub version: u32,
    pub namespace: NamespaceSnapshot,
    pub resources: Vec<ProviderResourceReference>,
    pub files: Vec<ProviderFileCheckpoint>,
    pub client: ProviderClientCheckpoint,
}

pub trait ProviderCheckpointCapture: Send + Sync {
    fn freeze(&self) -> Result<(), NamespaceError>;
    fn thaw(&self);

    fn resource_key(&self, slot: usize, remote: RemoteId) -> Result<ProviderResourceKey, NamespaceError>;

    /// Captures projected-file and quiesced client state. Implementations must
    /// reject live request waiters and callback executions.
    fn projected_state(&self) -> Result<(Vec<ProviderFileCheckpoint>, ProviderClientCheckpoint), NamespaceError>;
}

pub trait ProviderRemoteRestore: Send {
    fn remote(&mut self, key: ProviderResourceKey) -> Result<RemoteId, NamespaceError>;
    fn commit(&mut self) -> Result<(), NamespaceError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), NamespaceError>;
}

pub trait ProviderCheckpointReconnect: Send + Sync {
    fn stage(&self, image: &ProviderCheckpointImage) -> Result<Box<dyn ProviderRemoteRestore>, NamespaceError>;
}

impl ProviderCheckpointImage {
    pub fn capture(
        namespace: &HandleNamespace,
        capture: &dyn ProviderCheckpointCapture,
    ) -> Result<Self, NamespaceError> {
        let snapshot = namespace.checkpoint_snapshot()?;
        let resources = snapshot
            .entries
            .iter()
            .map(|entry| {
                capture
                    .resource_key(entry.slot, entry.remote)
                    .map(|key| ProviderResourceReference { slot: entry.slot, key })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (files, client) = capture.projected_state()?;
        let image = Self {
            version: PROVIDER_CHECKPOINT_VERSION,
            namespace: snapshot,
            resources,
            files,
            client,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn validate(&self) -> Result<(), NamespaceError> {
        if self.version != PROVIDER_CHECKPOINT_VERSION
            || self.namespace.capacity == 0
            || self.namespace.generations.len() != self.namespace.capacity
            || self.namespace.live != self.namespace.entries.len()
            || self.resources.len() != self.namespace.entries.len()
            || self.files.len() > PROVIDER_CHECKPOINT_FILE_MAXIMUM
            || self.client.next_request == 0
            || self.client.next_subscription == 0
        {
            return Err(NamespaceError::InvalidSnapshot);
        }
        let mut slots = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut references = 0_u64;
        let mut previous_slot = None;
        for entry in &self.namespace.entries {
            if entry.slot >= self.namespace.capacity
                || entry.generation == 0
                || self.namespace.generations[entry.slot] != entry.generation
                || entry.references == 0
                || !slots.insert(entry.slot)
                || !Self::slot_follows(previous_slot, entry.slot)
            {
                return Err(NamespaceError::InvalidSnapshot);
            }
            previous_slot = Some(entry.slot);
            references = references
                .checked_add(u64::from(entry.references))
                .ok_or(NamespaceError::InvalidSnapshot)?;
        }
        if references != self.namespace.references {
            return Err(NamespaceError::InvalidSnapshot);
        }
        let mut previous_resource = None;
        let mut resource_slots = BTreeMap::new();
        for resource in &self.resources {
            if !slots.contains(&resource.slot)
                || !keys.insert(resource.key)
                || !Self::slot_follows(previous_resource, resource.slot)
            {
                return Err(NamespaceError::InvalidSnapshot);
            }
            resource_slots.insert(resource.key, resource.slot);
            previous_resource = Some(resource.slot);
        }
        let mut descriptors = BTreeSet::new();
        let mut previous_descriptor = None;
        for file in &self.files {
            let resource_slot = resource_slots
                .get(&file.resource)
                .copied()
                .ok_or(NamespaceError::InvalidSnapshot)?;
            let resource_entry = self
                .namespace
                .entries
                .iter()
                .find(|entry| entry.slot == resource_slot)
                .ok_or(NamespaceError::InvalidSnapshot)?;
            if resource_entry.kind != crate::HandleKind::File
                || resource_entry.remote != file.snapshot.remote
                || !descriptors.insert(file.descriptor)
                || !Self::key_follows(previous_descriptor, file.descriptor)
                || file.snapshot.path.len() > PROVIDER_CHECKPOINT_PATH_BYTE_MAXIMUM
            {
                return Err(NamespaceError::InvalidSnapshot);
            }
            previous_descriptor = Some(file.descriptor);
        }
        if self.client.subscriptions.len() > self.client.subscription_generations.len() {
            return Err(NamespaceError::InvalidSnapshot);
        }
        self.validate_subscriptions()
    }

    fn validate_subscriptions(&self) -> Result<(), NamespaceError> {
        let mut subscription_keys = BTreeSet::new();
        let mut subscription_slots = BTreeSet::new();
        let mut previous_slot = None;
        for subscription in &self.client.subscriptions {
            if subscription.slot >= self.client.subscription_generations.len()
                || self.client.subscription_generations[subscription.slot] != subscription.key_generation
                || !subscription_slots.insert(subscription.slot)
                || !Self::slot_follows(previous_slot, subscription.slot)
                || subscription.identity_owner == 0
                || subscription.identity_generation == 0
                || subscription.key_id == 0
                || subscription.key_generation == 0
                || !subscription_keys.insert((subscription.key_id, subscription.key_generation))
            {
                return Err(NamespaceError::InvalidSnapshot);
            }
            previous_slot = Some(subscription.slot);
        }
        let mut event_bytes = 0_usize;
        for subscription in &self.client.subscriptions {
            for event in &subscription.queued {
                event_bytes = Self::event_total(event_bytes, event.len())?;
            }
        }
        Ok(())
    }

    fn event_total(current: usize, additional: usize) -> Result<usize, NamespaceError> {
        let total = current.checked_add(additional).ok_or(NamespaceError::InvalidSnapshot)?;
        if total > PROVIDER_CHECKPOINT_EVENT_BYTE_MAXIMUM {
            return Err(NamespaceError::InvalidSnapshot);
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
}
