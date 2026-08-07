use crate::{FakeHost, FakeHostError, ResourceKind};
use hl_vfs::{GuestName, MountSourceId, NodeHandle, NodeKind, ResolveHostError, VfsHost, XattrName};
use std::collections::BTreeMap;
use std::sync::Mutex;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InodeIdentity {
    pub inode: u64,
    pub generation: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchEvent {
    pub sequence: u64,
    pub parent: InodeIdentity,
    pub name: Vec<u8>,
    pub operation: &'static str,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    pub identity: InodeIdentity,
    pub kind: NodeKind,
    pub permissions: u16,
    pub owner: (u32, u32),
    pub size: usize,
    pub links: usize,
}
struct Node {
    identity: InodeIdentity,
    kind: NodeKind,
    permissions: u16,
    owner: (u32, u32),
    data: Vec<u8>,
    target: Vec<u8>,
    entries: BTreeMap<Vec<u8>, u64>,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
    links: usize,
}
struct TreeState {
    next_inode: u64,
    generation: u64,
    watch_sequence: u64,
    nodes: BTreeMap<u64, Node>,
    pins: BTreeMap<u64, InodeIdentity>,
    watches: Vec<WatchEvent>,
}
pub struct Tree {
    host: FakeHost,
    state: Mutex<TreeState>,
}
impl Tree {
    #[must_use]
    pub fn new(host: FakeHost) -> Self {
        let root = Node {
            identity: InodeIdentity {
                inode: 1,
                generation: 1,
            },
            kind: NodeKind::Directory,
            permissions: 0o755,
            owner: (0, 0),
            data: Vec::new(),
            target: Vec::new(),
            entries: BTreeMap::new(),
            xattrs: BTreeMap::new(),
            links: 1,
        };
        Self {
            host,
            state: Mutex::new(TreeState {
                next_inode: 2,
                generation: 1,
                watch_sequence: 0,
                nodes: BTreeMap::from([(1, root)]),
                pins: BTreeMap::new(),
                watches: Vec::new(),
            }),
        }
    }
    pub fn mkdir(&self, parent: InodeIdentity, name: &str) -> Result<InodeIdentity, FakeHostError> {
        self.create(parent, name, NodeKind::Directory, Vec::new())
    }
    pub fn create_file(
        &self,
        parent: InodeIdentity,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<InodeIdentity, FakeHostError> {
        self.create(parent, name, NodeKind::File, bytes)
    }

    pub fn symlink(&self, parent: InodeIdentity, name: &str, target: &[u8]) -> Result<InodeIdentity, FakeHostError> {
        self.create(parent, name, NodeKind::Symlink, target.to_vec())
    }

    pub fn link(&self, parent: InodeIdentity, name: &str, target: InodeIdentity) -> Result<(), FakeHostError> {
        self.host.record("vfs", "link", parent.inode, 0, 0)?;
        let mut state = self.lock();
        Self::node(&state, target)?;
        let directory = Self::directory_mut(&mut state, parent)?;
        if directory.entries.contains_key(name.as_bytes()) {
            return Err(FakeHostError::invalid("vfs", parent.inode));
        }
        directory.entries.insert(name.as_bytes().to_vec(), target.inode);
        state.nodes.get_mut(&target.inode).expect("validated target").links += 1;
        Self::watch(&mut state, parent, name, "link");
        Ok(())
    }

    pub fn rename(
        &self,
        source_parent: InodeIdentity,
        source: &str,
        target_parent: InodeIdentity,
        target: &str,
    ) -> Result<(), FakeHostError> {
        self.host.record("vfs", "rename", source_parent.inode, 0, 0)?;
        let mut state = self.lock();
        let inode = Self::directory_node(&state, source_parent)?
            .entries
            .get(source.as_bytes())
            .copied()
            .ok_or(FakeHostError::invalid("vfs", source_parent.inode))?;
        if Self::directory_node(&state, target_parent)?
            .entries
            .contains_key(target.as_bytes())
        {
            return Err(FakeHostError::invalid("vfs", target_parent.inode));
        }
        Self::directory_mut(&mut state, source_parent)?
            .entries
            .remove(source.as_bytes());
        Self::directory_mut(&mut state, target_parent)?
            .entries
            .insert(target.as_bytes().to_vec(), inode);
        Self::watch(&mut state, source_parent, source, "rename-from");
        Self::watch(&mut state, target_parent, target, "rename-to");
        Ok(())
    }

    pub fn unlink(&self, parent: InodeIdentity, name: &str) -> Result<(), FakeHostError> {
        self.host.record("vfs", "unlink", parent.inode, 0, 0)?;
        let mut state = self.lock();
        let inode = Self::directory_mut(&mut state, parent)?
            .entries
            .remove(name.as_bytes())
            .ok_or(FakeHostError::invalid("vfs", parent.inode))?;
        let node = state.nodes.get_mut(&inode).expect("directory owned node");
        node.links -= 1;
        if node.links == 0 && !state.pins.values().any(|pin| pin.inode == inode) {
            state.nodes.remove(&inode);
        }
        Self::watch(&mut state, parent, name, "unlink");
        Ok(())
    }

    pub fn set_permissions(&self, identity: InodeIdentity, permissions: u16) -> Result<(), FakeHostError> {
        self.host.record("vfs", "chmod", identity.inode, 0, 0)?;
        Self::node_mut(&mut self.lock(), identity)?.permissions = permissions & 0o7777;
        Ok(())
    }

    pub fn set_owner(&self, identity: InodeIdentity, uid: u32, gid: u32) -> Result<(), FakeHostError> {
        self.host.record("vfs", "chown", identity.inode, 0, 0)?;
        Self::node_mut(&mut self.lock(), identity)?.owner = (uid, gid);
        Ok(())
    }

    pub fn set_xattr(&self, identity: InodeIdentity, name: &XattrName, value: Vec<u8>) -> Result<(), FakeHostError> {
        self.host
            .record("vfs", "setxattr", identity.inode, value.len(), value.len())?;
        Self::node_mut(&mut self.lock(), identity)?
            .xattrs
            .insert(name.as_bytes().to_vec(), value);
        Ok(())
    }

    pub fn metadata(&self, identity: InodeIdentity) -> Result<NodeMetadata, FakeHostError> {
        let state = self.lock();
        let node = Self::node(&state, identity)?;
        Ok(NodeMetadata {
            identity: node.identity,
            kind: node.kind,
            permissions: node.permissions,
            owner: node.owner,
            size: node.data.len(),
            links: node.links,
        })
    }

    pub fn read_file(&self, identity: InodeIdentity) -> Result<Vec<u8>, FakeHostError> {
        let state = self.lock();
        let node = Self::node(&state, identity)?;
        (node.kind == NodeKind::File)
            .then(|| node.data.clone())
            .ok_or(FakeHostError::invalid("vfs", identity.inode))
    }

    pub fn xattr(&self, identity: InodeIdentity, name: &XattrName) -> Result<Option<Vec<u8>>, FakeHostError> {
        Ok(Self::node(&self.lock(), identity)?.xattrs.get(name.as_bytes()).cloned())
    }

    pub fn resolve(&self, path: &str) -> Result<InodeIdentity, ResolveHostError> {
        let mut components = path.split('/').filter(|component| !component.is_empty());
        let mut current = InodeIdentity {
            inode: 1,
            generation: 1,
        };
        let mut followed = 0;
        while let Some(component) = components.next() {
            if component == "." {
                continue;
            }
            if component == ".." {
                return Err(ResolveHostError::PermissionDenied);
            }
            let (identity, target) = self.resolve_component(current, component)?;
            current = identity;
            if let Some(target) = target {
                followed += 1;
                Self::validate_symlink_suffix(followed, components.next())?;
                return self.resolve(&target);
            }
        }
        Ok(current)
    }

    #[must_use]
    pub fn directory(&self, identity: InodeIdentity) -> Vec<(u64, String, InodeIdentity)> {
        let state = self.lock();
        let Ok(directory) = Self::directory_node(&state, identity) else {
            return Vec::new();
        };
        directory
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, (name, inode))| {
                state.nodes.get(inode).map(|node| {
                    (
                        index as u64 + 1,
                        String::from_utf8(name.clone()).expect("text creation API stores UTF-8"),
                        node.identity,
                    )
                })
            })
            .collect()
    }

    #[must_use]
    pub fn watch_events(&self) -> Vec<WatchEvent> {
        self.lock().watches.clone()
    }

    fn create(
        &self,
        parent: InodeIdentity,
        name: &str,
        kind: NodeKind,
        payload: Vec<u8>,
    ) -> Result<InodeIdentity, FakeHostError> {
        self.host
            .record("vfs", "create", parent.inode, payload.len(), payload.len())?;
        let mut state = self.lock();
        if name.is_empty()
            || name.contains('/')
            || Self::directory_node(&state, parent)?
                .entries
                .contains_key(name.as_bytes())
        {
            return Err(FakeHostError::invalid("vfs", parent.inode));
        }
        let inode = state.next_inode;
        state.next_inode += 1;
        state.generation += 1;
        let identity = InodeIdentity {
            inode,
            generation: state.generation,
        };
        let (data, target) = if kind == NodeKind::Symlink {
            (Vec::new(), payload)
        } else {
            (payload, Vec::new())
        };
        state.nodes.insert(
            inode,
            Node {
                identity,
                kind,
                permissions: if kind == NodeKind::Directory { 0o755 } else { 0o644 },
                owner: (0, 0),
                data,
                target,
                entries: BTreeMap::new(),
                xattrs: BTreeMap::new(),
                links: 1,
            },
        );
        Self::directory_mut(&mut state, parent)?
            .entries
            .insert(name.as_bytes().to_vec(), inode);
        Self::watch(&mut state, parent, name, "create");
        Ok(identity)
    }

    fn resolve_component(
        &self,
        parent: InodeIdentity,
        component: &str,
    ) -> Result<(InodeIdentity, Option<String>), ResolveHostError> {
        let state = self.lock();
        let inode = Self::directory_node(&state, parent)
            .map_err(|_| ResolveHostError::NotDirectory)?
            .entries
            .get(component.as_bytes())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let node = state.nodes.get(&inode).ok_or(ResolveHostError::NotFound)?;
        let target = (node.kind == NodeKind::Symlink)
            .then(|| std::str::from_utf8(&node.target).map(str::to_owned))
            .transpose()
            .map_err(|_| ResolveHostError::Io)?;
        Ok((node.identity, target))
    }

    fn validate_symlink_suffix(followed: usize, remaining: Option<&str>) -> Result<(), ResolveHostError> {
        (followed <= 40 && remaining.is_none())
            .then_some(())
            .ok_or(ResolveHostError::Io)
    }

    fn watch(state: &mut TreeState, parent: InodeIdentity, name: &str, operation: &'static str) {
        state.watch_sequence += 1;
        state.watches.push(WatchEvent {
            sequence: state.watch_sequence,
            parent,
            name: name.as_bytes().to_vec(),
            operation,
        });
    }

    fn node(state: &TreeState, identity: InodeIdentity) -> Result<&Node, FakeHostError> {
        state
            .nodes
            .get(&identity.inode)
            .filter(|node| node.identity == identity)
            .ok_or(FakeHostError::invalid("vfs", identity.inode))
    }

    fn node_mut(state: &mut TreeState, identity: InodeIdentity) -> Result<&mut Node, FakeHostError> {
        state
            .nodes
            .get_mut(&identity.inode)
            .filter(|node| node.identity == identity)
            .ok_or(FakeHostError::invalid("vfs", identity.inode))
    }

    fn directory_node(state: &TreeState, identity: InodeIdentity) -> Result<&Node, FakeHostError> {
        Self::node(state, identity).and_then(|node| {
            (node.kind == NodeKind::Directory)
                .then_some(node)
                .ok_or(FakeHostError::invalid("vfs", identity.inode))
        })
    }

    fn directory_mut(state: &mut TreeState, identity: InodeIdentity) -> Result<&mut Node, FakeHostError> {
        Self::node_mut(state, identity).and_then(|node| {
            (node.kind == NodeKind::Directory)
                .then_some(node)
                .ok_or(FakeHostError::invalid("vfs", identity.inode))
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TreeState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl VfsHost for Tree {
    type ParentLease = NodeHandle;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        let resource = self
            .host
            .allocate("vfs-pin", ResourceKind::Pin)
            .map_err(|_| ResolveHostError::ResourceLimit)?;
        self.lock().pins.insert(
            resource,
            InodeIdentity {
                inode: 1,
                generation: 1,
            },
        );
        Ok(NodeHandle::from_raw(resource))
    }

    fn pin_mount(&self, source: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        if source.get() == 1 {
            self.pin_root()
        } else {
            Err(ResolveHostError::NotFound)
        }
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        let mut state = self.lock();
        let parent = state
            .pins
            .get(&directory.raw())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let inode = Self::directory_node(&state, parent)
            .map_err(|_| ResolveHostError::NotDirectory)?
            .entries
            .get(component.as_bytes())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let node = state.nodes.get(&inode).ok_or(ResolveHostError::NotFound)?;
        let identity = node.identity;
        let kind = node.kind;
        let resource = self
            .host
            .allocate("vfs-pin", ResourceKind::Pin)
            .map_err(|_| ResolveHostError::ResourceLimit)?;
        state.pins.insert(resource, identity);
        Ok((NodeHandle::from_raw(resource), kind))
    }

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let state = self.lock();
        let identity = state.pins.get(&link.raw()).copied().ok_or(ResolveHostError::NotFound)?;
        let node = Self::node(&state, identity).map_err(|_| ResolveHostError::NotFound)?;
        if node.kind != NodeKind::Symlink {
            return Err(ResolveHostError::Io);
        }
        let count = output.len().min(node.target.len());
        output[..count].copy_from_slice(&node.target[..count]);
        Ok(count)
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        let identity = self
            .lock()
            .pins
            .get(&parent.raw())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let resource = self
            .host
            .allocate("vfs-pin", ResourceKind::Pin)
            .map_err(|_| ResolveHostError::ResourceLimit)?;
        self.lock().pins.insert(resource, identity);
        Ok(NodeHandle::from_raw(resource))
    }

    fn close(&self, node: NodeHandle) {
        let mut state = self.lock();
        let removed = state.pins.remove(&node.raw());
        if let Some(identity) = removed {
            let still_pinned = state.pins.values().any(|pin| pin.inode == identity.inode);
            let unlinked = state
                .nodes
                .get(&identity.inode)
                .is_some_and(|candidate| candidate.links == 0);
            if unlinked && !still_pinned {
                state.nodes.remove(&identity.inode);
            }
            drop(state);
            let _ = self.host.release("vfs-pin", ResourceKind::Pin, node.raw());
        }
    }
}
