use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::{
    AccessIdentity, Capabilities, GuestName, GuestPathBytes, Identity, Kind, Metadata, MountKind, MountNamespace,
    MountSourceId, MutationAction, MutationError, MutationHostError, NodeHandle, NodeKind, Permissions, PinnedParent,
    ReadOnlyPaths, RenameFlags, ResolveHostError, Timestamp, Umask, VfsHost, VfsMutationHost, VfsMutations,
    VfsTransaction,
};

#[derive(Clone)]
struct FakeHost {
    state: Arc<Mutex<FixtureState>>,
}

struct Node {
    metadata: Metadata,
    children: BTreeMap<Vec<u8>, u64>,
}

struct FixtureState {
    nodes: Vec<Node>,
    pins: HashMap<u64, u64>,
    next_pin: u64,
    next_transaction: u64,
    staged: HashMap<u64, Vec<MutationAction>>,
    published: Vec<MutationAction>,
    transcript: Vec<String>,
    fail_stage: bool,
    fail_commit: bool,
}

impl FakeHost {
    fn new() -> Self {
        let host = Self {
            state: Arc::new(Mutex::new(FixtureState {
                nodes: vec![Node {
                    metadata: metadata(Kind::Directory, 0, 0, 0o777),
                    children: BTreeMap::new(),
                }],
                pins: HashMap::new(),
                next_pin: 1,
                next_transaction: 1,
                staged: HashMap::new(),
                published: Vec::new(),
                transcript: Vec::new(),
                fail_stage: false,
                fail_commit: false,
            })),
        };
        host.add(0, "tmp", Kind::Directory, 0, 0, 0o777);
        host
    }

    fn add(&self, parent: u64, name: impl AsRef<[u8]>, kind: Kind, user: u32, group: u32, mode: u16) -> u64 {
        let mut state = self.state.lock().unwrap();
        let node = state.nodes.len() as u64;
        state.nodes.push(Node {
            metadata: metadata(kind, user, group, mode),
            children: BTreeMap::new(),
        });
        state.nodes[parent as usize]
            .children
            .insert(name.as_ref().to_vec(), node);
        node
    }

    fn node(&self, path: &str) -> u64 {
        let state = self.state.lock().unwrap();
        path.split('/')
            .filter(|part| !part.is_empty())
            .fold(0, |node, part| state.nodes[node as usize].children[part.as_bytes()])
    }

    fn pin(state: &mut FixtureState, node: u64) -> NodeHandle {
        let handle = state.next_pin;
        state.next_pin += 1;
        state.pins.insert(handle, node);
        NodeHandle::from_raw(handle)
    }

    fn pinned(state: &FixtureState, handle: NodeHandle) -> Result<u64, ResolveHostError> {
        state.pins.get(&handle.raw()).copied().ok_or(ResolveHostError::Io)
    }

    fn identity(user: u32) -> AccessIdentity {
        AccessIdentity {
            user,
            group: user,
            supplementary_groups: Vec::new(),
            capabilities: Capabilities::default(),
        }
    }

    fn fail_stage(&self) {
        self.state.lock().unwrap().fail_stage = true;
    }

    fn fail_commit(&self) {
        self.state.lock().unwrap().fail_commit = true;
    }

    fn transcript(&self) -> Vec<String> {
        self.state.lock().unwrap().transcript.clone()
    }

    fn published(&self) -> Vec<MutationAction> {
        self.state.lock().unwrap().published.clone()
    }
}

impl VfsHost for FakeHost {
    type ParentLease = NodeHandle;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        Ok(Self::pin(&mut state, 0))
    }

    fn pin_mount(&self, _source: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        Err(ResolveHostError::NotFound)
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let directory = Self::pinned(&state, directory)?;
        let child = state.nodes[directory as usize]
            .children
            .get(component.as_bytes())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let kind = match state.nodes[child as usize].metadata.kind {
            Kind::Directory => NodeKind::Directory,
            Kind::Symlink => NodeKind::Symlink,
            Kind::Regular => NodeKind::File,
            _ => NodeKind::Other,
        };
        Ok((Self::pin(&mut state, child), kind))
    }

    fn read_link(&self, _link: NodeHandle, _output: &mut [u8]) -> Result<usize, ResolveHostError> {
        Err(ResolveHostError::Io)
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let node = Self::pinned(&state, parent)?;
        Ok(Self::pin(&mut state, node))
    }

    fn close(&self, node: NodeHandle) {
        assert!(self.state.lock().unwrap().pins.remove(&node.raw()).is_some());
    }
}

impl VfsMutationHost for FakeHost {
    fn metadata_node(&self, node: NodeHandle) -> Result<Metadata, MutationHostError> {
        let state = self.state.lock().unwrap();
        let node = state.pins.get(&node.raw()).copied().ok_or(MutationHostError::Race)?;
        Ok(state.nodes[node as usize].metadata.clone())
    }

    fn metadata_at(
        &self,
        parent: NodeHandle,
        name: &GuestName,
        _nofollow: bool,
    ) -> Result<Option<Metadata>, MutationHostError> {
        let state = self.state.lock().unwrap();
        let parent = state.pins.get(&parent.raw()).copied().ok_or(MutationHostError::Race)?;
        let node = state.nodes[parent as usize].children.get(name.as_bytes()).copied();
        Ok(node.map(|node| state.nodes[node as usize].metadata.clone()))
    }

    fn begin(&self, parents: &[PinnedParent<'_>]) -> Result<VfsTransaction, MutationHostError> {
        let mut state = self.state.lock().unwrap();
        if parents
            .iter()
            .any(|parent| !state.pins.contains_key(&parent.node.raw()))
        {
            return Err(MutationHostError::Race);
        }
        let transaction = state.next_transaction;
        state.next_transaction += 1;
        state.staged.insert(transaction, Vec::new());
        state.transcript.push(format!("begin:{transaction}"));
        Ok(VfsTransaction::from_raw(transaction))
    }

    fn stage(&self, transaction: VfsTransaction, action: &MutationAction) -> Result<(), MutationHostError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_stage {
            return Err(MutationHostError::Io);
        }
        state
            .staged
            .get_mut(&transaction.raw())
            .ok_or(MutationHostError::Race)?
            .push(action.clone());
        state.transcript.push(format!("stage:{}", transaction.raw()));
        Ok(())
    }

    fn commit(&self, transaction: VfsTransaction) -> Result<(), MutationHostError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_commit {
            return Err(MutationHostError::Io);
        }
        let staged = state.staged.remove(&transaction.raw()).ok_or(MutationHostError::Race)?;
        state.published.extend(staged);
        state.transcript.push(format!("commit:{}", transaction.raw()));
        Ok(())
    }

    fn rollback(&self, transaction: VfsTransaction) {
        let mut state = self.state.lock().unwrap();
        state.staged.remove(&transaction.raw());
        state.transcript.push(format!("rollback:{}", transaction.raw()));
    }
}

fn metadata(kind: Kind, user: u32, group: u32, mode: u16) -> Metadata {
    let timestamp = Timestamp::new(0, 0).unwrap();
    Metadata {
        identity: Identity { device: 1, inode: 1 },
        kind,
        permissions: Permissions::from_bits(mode),
        links: 1,
        user,
        group,
        special_device: 0,
        size: 0,
        blocks_512: 0,
        block_size: 4096,
        accessed: timestamp,
        modified: timestamp,
        changed: timestamp,
    }
}

#[test]
fn rename_flag_work() {
    assert_eq!(RenameFlags::from_bits(3), Err(MutationError::InvalidArgument));
    assert_eq!(RenameFlags::from_bits(0x40), Err(MutationError::InvalidArgument));
}

#[test]
fn mkdir_stage_publication() {
    for commit in [false, true] {
        let host = FakeHost::new();
        if commit {
            host.fail_commit();
        } else {
            host.fail_stage();
        }
        let namespace = MountNamespace::new();
        let readonly = ReadOnlyPaths::new();
        let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
        let result = service.mkdir(
            &GuestPathBytes::new(b"/tmp/new").unwrap(),
            Permissions::from_bits(0o777),
            &FakeHost::identity(0),
            &Umask::new(0o022),
        );
        assert_eq!(result, Err(MutationError::Host(MutationHostError::Io)));
        assert!(host.published().is_empty());
        assert!(host.transcript().iter().any(|line| line.starts_with("rollback")));
    }
}

#[test]
fn sticky_directory_transaction() {
    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.state.lock().unwrap().nodes[tmp as usize].metadata.permissions = Permissions::from_bits(0o1777);
    host.add(tmp, "owned", Kind::Regular, 20, 20, 0o666);
    let namespace = MountNamespace::new();
    let readonly = ReadOnlyPaths::new();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    assert_eq!(
        service.unlink(&GuestPathBytes::new(b"/tmp/owned").unwrap(), &FakeHost::identity(30)),
        Err(MutationError::OperationNotPermitted)
    );
    assert!(host.transcript().is_empty());
}

#[test]
fn cross_mount_begin() {
    let host = FakeHost::new();
    let namespace = MountNamespace::new();
    let source = MountSourceId::new(7).unwrap();
    namespace
        .mount("/tmp/mounted", source, MountKind::Directory, false)
        .unwrap();
    let readonly = ReadOnlyPaths::new();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    assert_eq!(
        service.rename(
            &GuestPathBytes::new(b"/tmp/a").unwrap(),
            &GuestPathBytes::new(b"/tmp/mounted/a").unwrap(),
            RenameFlags::default(),
            &FakeHost::identity(0),
        ),
        Err(MutationError::CrossMount)
    );
    assert!(host.transcript().is_empty());
}

#[test]
fn overlay_exchange_pair() {
    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.add(tmp, "a", Kind::Directory, 0, 0, 0o777);
    host.add(tmp, "b", Kind::Directory, 0, 0, 0o777);
    let namespace = MountNamespace::new();
    let readonly = ReadOnlyPaths::new();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, true);
    let events = service
        .rename(
            &GuestPathBytes::new(b"/tmp/a").unwrap(),
            &GuestPathBytes::new(b"/tmp/b").unwrap(),
            RenameFlags::from_bits(u32::from(RenameFlags::EXCHANGE)).unwrap(),
            &FakeHost::identity(0),
        )
        .unwrap();
    let actions = host.published();
    assert!(matches!(actions[0], MutationAction::CopyUp { recursive: true, .. }));
    assert!(matches!(actions[1], MutationAction::CopyUp { recursive: true, .. }));
    assert!(matches!(actions[2], MutationAction::Rename { .. }));
    assert_eq!(events.len(), 2);
    assert_eq!(
        host.transcript(),
        vec!["begin:1", "stage:1", "stage:1", "stage:1", "commit:1"]
    );
}

#[test]
fn invalid_byte_rollback() {
    let namespace = MountNamespace::new();
    let readonly = ReadOnlyPaths::new();
    let identity = FakeHost::identity(0);

    let host = FakeHost::new();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    let created = GuestPathBytes::new(b"/tmp/\xff").unwrap();
    let events = service
        .mkdir(&created, Permissions::from_bits(0o777), &identity, &Umask::new(0))
        .unwrap();
    assert_eq!(
        events,
        [crate::WatchEvent::Created {
            path: created.clone(),
            directory: true,
        }]
    );
    assert!(matches!(
        &host.published()[0],
        MutationAction::Create { name, .. } if name.as_bytes() == b"\xff"
    ));

    let host = FakeHost::new();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    let link_path = GuestPathBytes::new(b"/tmp/\xfd").unwrap();
    let link_target = GuestPathBytes::new(b"../\xfe").unwrap();
    service.symlink(&link_target, &link_path, &identity).unwrap();
    assert!(matches!(
        &host.published()[0],
        MutationAction::Symlink { name, target, .. }
            if name.as_bytes() == b"\xfd" && target.as_bytes() == b"../\xfe"
    ));

    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.add(tmp, b"\xfc", Kind::Regular, 0, 0, 0o666);
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    let removed = GuestPathBytes::new(b"/tmp/\xfc").unwrap();
    let events = service.unlink(&removed, &identity).unwrap();
    assert_eq!(
        events,
        [crate::WatchEvent::Deleted {
            path: removed,
            directory: false,
        }]
    );

    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.add(tmp, b"\xfb", Kind::Regular, 0, 0, 0o666);
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    service
        .link(
            &GuestPathBytes::new(b"/tmp/\xfb").unwrap(),
            &GuestPathBytes::new(b"/tmp/\xfa").unwrap(),
            false,
            &identity,
        )
        .unwrap();
    assert!(matches!(
        &host.published()[0],
        MutationAction::HardLink { source_name, target_name, .. }
            if source_name.as_bytes() == b"\xfb"
                && target_name.as_bytes() == b"\xfa"
    ));

    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.add(tmp, b"\xf9", Kind::Regular, 0, 0, 0o666);
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    let source = GuestPathBytes::new(b"/tmp/\xf9").unwrap();
    let target = GuestPathBytes::new(b"/tmp/\xf8").unwrap();
    let events = service
        .rename(&source, &target, RenameFlags::default(), &identity)
        .unwrap();
    assert!(matches!(
        &host.published()[0],
        MutationAction::Rename { source_name, target_name, .. }
            if source_name.as_bytes() == b"\xf9"
                && target_name.as_bytes() == b"\xf8"
    ));
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], crate::WatchEvent::MovedFrom { path, .. } if path == &source));
    assert!(matches!(&events[1], crate::WatchEvent::MovedTo { path, .. } if path == &target));

    let host = FakeHost::new();
    let tmp = host.node("/tmp");
    host.add(tmp, b"\xf9", Kind::Regular, 0, 0, 0o666);
    host.fail_stage();
    let service = VfsMutations::new(host.clone(), &namespace, &readonly, false, false);
    let result = service.rename(
        &GuestPathBytes::new(b"/tmp/\xf9").unwrap(),
        &GuestPathBytes::new(b"/tmp/\xf8").unwrap(),
        RenameFlags::default(),
        &identity,
    );
    assert_eq!(result, Err(MutationError::Host(MutationHostError::Io)));
    assert!(host.published().is_empty());
    assert!(host.transcript().iter().any(|entry| entry.starts_with("rollback")));
}
