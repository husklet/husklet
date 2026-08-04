use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::device::{Error, Id, Node, NodeId, NodeKind, Scope};
use crate::{GuestPathBytes, MountRoute, Permissions, ProjectedObjectId};

const DEVICE_MAXIMUM: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Registration {
    pub path: GuestPathBytes,
    pub scope: Scope,
    pub device: Id,
    pub kind: NodeKind,
    pub permissions: Permissions,
    pub user: u32,
    pub group: u32,
    pub object: ProjectedObjectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub id: NodeId,
    pub registration: Registration,
}

#[derive(Default)]
struct Slot {
    generation: u64,
    node: Option<Arc<Node>>,
}

#[derive(Default)]
struct State {
    slots: Vec<Slot>,
    paths: BTreeMap<(ScopeKey, Vec<u8>), usize>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ScopeKey {
    Root,
    Mounted(u64),
}

impl From<Scope> for ScopeKey {
    fn from(value: Scope) -> Self {
        match value {
            Scope::Root => Self::Root,
            Scope::Mounted(source) => Self::Mounted(source.get()),
        }
    }
}

pub struct Registry {
    state: RwLock<State>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: RwLock::new(State::default()),
        }
    }

    pub fn register(&self, registration: Registration) -> Result<Arc<Node>, Error> {
        if !registration.path.is_absolute() {
            return Err(Error::RelativePath);
        }
        let key = (registration.scope.into(), registration.path.as_bytes().to_vec());
        let mut state = self.state.write().unwrap_or_else(|error| error.into_inner());
        if state.paths.contains_key(&key) {
            return Err(Error::Duplicate);
        }
        let index = state
            .slots
            .iter()
            .position(|slot| slot.node.is_none())
            .unwrap_or(state.slots.len());
        if index == DEVICE_MAXIMUM {
            return Err(Error::Capacity);
        }
        if index == state.slots.len() {
            state.slots.push(Slot::default());
        }
        let slot = &mut state.slots[index];
        slot.generation = slot.generation.checked_add(1).ok_or(Error::Capacity)?;
        let id = NodeId {
            slot: u16::try_from(index + 1).expect("bounded device slot"),
            generation: slot.generation,
        };
        let node = Arc::new(Node {
            id,
            path: registration.path,
            scope: registration.scope,
            device: registration.device,
            kind: registration.kind,
            permissions: registration.permissions,
            user: registration.user,
            group: registration.group,
            object: registration.object,
        });
        slot.node = Some(Arc::clone(&node));
        state.paths.insert(key, index);
        Ok(node)
    }

    pub fn restore(snapshots: &[Snapshot]) -> Result<Self, Error> {
        let mut state = State::default();
        for snapshot in snapshots {
            let index = usize::from(snapshot.id.slot)
                .checked_sub(1)
                .ok_or(Error::InvalidSnapshot)?;
            if index >= DEVICE_MAXIMUM || snapshot.id.generation == 0 || !snapshot.registration.path.is_absolute() {
                return Err(Error::InvalidSnapshot);
            }
            let key = (
                snapshot.registration.scope.into(),
                snapshot.registration.path.as_bytes().to_vec(),
            );
            if state.paths.contains_key(&key) {
                return Err(Error::InvalidSnapshot);
            }
            state.slots.resize_with(index + 1, Slot::default);
            if state.slots[index].node.is_some() {
                return Err(Error::InvalidSnapshot);
            }
            let registration = &snapshot.registration;
            let node = Arc::new(Node {
                id: snapshot.id,
                path: registration.path.clone(),
                scope: registration.scope,
                device: registration.device,
                kind: registration.kind,
                permissions: registration.permissions,
                user: registration.user,
                group: registration.group,
                object: registration.object,
            });
            state.slots[index] = Slot {
                generation: snapshot.id.generation,
                node: Some(node),
            };
            state.paths.insert(key, index);
        }
        Ok(Self {
            state: RwLock::new(state),
        })
    }

    pub fn remove(&self, id: NodeId) -> Result<(), Error> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(Error::Stale)?;
        let mut state = self.state.write().unwrap_or_else(|error| error.into_inner());
        let slot = state.slots.get_mut(index).ok_or(Error::Stale)?;
        let node = slot.node.as_ref().filter(|node| node.id == id).ok_or(Error::Stale)?;
        let key = (node.scope.into(), node.path.as_bytes().to_vec());
        slot.node = None;
        state.paths.remove(&key);
        Ok(())
    }

    #[must_use]
    pub fn lookup(&self, path: &GuestPathBytes, route: MountRoute) -> Option<Arc<Node>> {
        let scope = match route {
            MountRoute::Root => ScopeKey::Root,
            MountRoute::Mounted { source, .. } => ScopeKey::Mounted(source.get()),
        };
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        state
            .paths
            .get(&(scope, path.as_bytes().to_vec()))
            .and_then(|index| state.slots[*index].node.as_ref())
            .map(Arc::clone)
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<Snapshot> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .slots
            .iter()
            .filter_map(|slot| slot.node.as_ref())
            .map(|node| Snapshot {
                id: node.id,
                registration: Registration {
                    path: node.path.clone(),
                    scope: node.scope,
                    device: node.device,
                    kind: node.kind,
                    permissions: node.permissions,
                    user: node.user,
                    group: node.group,
                    object: node.object,
                },
            })
            .collect()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
