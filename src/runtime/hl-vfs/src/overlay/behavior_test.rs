use crate::{
    CopyContent, CopyUpPlan, CreatePlan, DirectoryEntry, GuestName, GuestPathBytes, Layer, LayerEntry, MutationHandle,
    NodeHandle, NodeMetadata, Overlay, OverlayError, OverlayHost, OverlayLookup, OverlayNodeKind, ResolveHostError,
    VfsHost,
};
use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
#[derive(Clone)]
struct FakeOverlayHost {
    state: Arc<Mutex<FakeOverlayState>>,
}
struct FakeOverlayState {
    entries: HashMap<(Layer, Vec<u8>), LayerEntry>,
    directories: HashMap<(Layer, Vec<u8>), Vec<DirectoryEntry>>,
    staged: HashMap<u64, (Vec<u8>, Option<LayerEntry>)>,
    next_mutation: u64,
    failure: Option<FailurePoint>,
    gate: Option<Arc<CommitGate>>,
    transcript: Vec<String>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum FailurePoint {
    Parents,
    Stage,
    Commit,
}
struct CommitGate {
    staged: Barrier,
    release: Barrier,
}
impl FakeOverlayHost {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeOverlayState {
                entries: HashMap::new(),
                directories: HashMap::new(),
                staged: HashMap::new(),
                next_mutation: 1,
                failure: None,
                gate: None,
                transcript: Vec::new(),
            })),
        }
    }
    fn put(&self, layer: Layer, path: &str, entry: LayerEntry) {
        self.put_bytes(layer, path.as_bytes(), entry);
    }
    fn put_bytes(&self, layer: Layer, path: &[u8], entry: LayerEntry) {
        self.state.lock().unwrap().entries.insert((layer, path.to_vec()), entry);
    }
    fn directory(&self, layer: Layer, path: &str, entries: Vec<DirectoryEntry>) {
        self.state
            .lock()
            .unwrap()
            .directories
            .insert((layer, path.as_bytes().to_vec()), entries);
    }
    fn fail_at(&self, failure: FailurePoint) {
        self.state.lock().unwrap().failure = Some(failure);
    }
    fn gate_commit(&self) -> Arc<CommitGate> {
        let gate = Arc::new(CommitGate {
            staged: Barrier::new(2),
            release: Barrier::new(2),
        });
        self.state.lock().unwrap().gate = Some(gate.clone());
        gate
    }
    fn transcript(&self) -> Vec<String> {
        self.state.lock().unwrap().transcript.clone()
    }
    fn published(&self, path: &str) -> LayerEntry {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(&(Layer::Upper, path.as_bytes().to_vec()))
            .cloned()
            .unwrap_or(LayerEntry::Absent)
    }
}
impl VfsHost for FakeOverlayHost {
    type ParentLease = NodeHandle;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        Ok(NodeHandle::from_raw(1))
    }
    fn pin_mount(&self, _source: crate::MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        Err(ResolveHostError::NotFound)
    }
    fn inspect_child(
        &self,
        _directory: NodeHandle,
        _component: &GuestName,
    ) -> Result<(NodeHandle, crate::NodeKind), ResolveHostError> {
        Err(ResolveHostError::NotFound)
    }
    fn read_link(&self, _link: NodeHandle, _output: &mut [u8]) -> Result<usize, ResolveHostError> {
        Err(ResolveHostError::NotFound)
    }
    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        Ok(parent)
    }
    fn close(&self, _node: NodeHandle) {}
}
impl OverlayHost for FakeOverlayHost {
    fn probe(&self, layer: Layer, path: &GuestPathBytes) -> Result<LayerEntry, ResolveHostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .entries
            .get(&(layer, path.as_bytes().to_vec()))
            .cloned()
            .unwrap_or(LayerEntry::Absent))
    }
    fn read_directory(&self, layer: Layer, path: &GuestPathBytes) -> Result<Vec<DirectoryEntry>, ResolveHostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .directories
            .get(&(layer, path.as_bytes().to_vec()))
            .cloned()
            .unwrap_or_default())
    }
    fn begin_mutation(&self, path: &GuestPathBytes) -> Result<MutationHandle, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let handle = state.next_mutation;
        state.next_mutation += 1;
        state.staged.insert(handle, (path.as_bytes().to_vec(), None));
        state.transcript.push(format!("begin:{handle}"));
        Ok(MutationHandle::from_raw(handle))
    }
    fn stage_parent_directories(
        &self,
        mutation: MutationHandle,
        _path: &GuestPathBytes,
    ) -> Result<(), ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        state.transcript.push(format!("parents:{}", mutation.raw()));
        if state.failure == Some(FailurePoint::Parents) {
            return Err(ResolveHostError::Io);
        }
        Ok(())
    }
    fn stage_copy_up(&self, mutation: MutationHandle, plan: &CopyUpPlan) -> Result<(), ResolveHostError> {
        self.stage_node(
            mutation,
            LayerEntry::Node {
                kind: plan.kind,
                metadata: plan.metadata.clone(),
                opaque: false,
            },
        )
    }
    fn stage_create(&self, mutation: MutationHandle, plan: &CreatePlan) -> Result<(), ResolveHostError> {
        self.stage_node(
            mutation,
            LayerEntry::Node {
                kind: plan.kind,
                metadata: plan.metadata.clone(),
                opaque: false,
            },
        )
    }
    fn commit_mutation(&self, mutation: MutationHandle) -> Result<(), ResolveHostError> {
        let gate = {
            let mut state = self.state.lock().unwrap();
            state.transcript.push(format!("commit:{}", mutation.raw()));
            state.gate.clone()
        };
        if let Some(gate) = gate {
            gate.staged.wait();
            gate.release.wait();
        }
        let mut state = self.state.lock().unwrap();
        if state.failure == Some(FailurePoint::Commit) {
            return Err(ResolveHostError::Io);
        }
        let (path, entry) = state.staged.remove(&mutation.raw()).ok_or(ResolveHostError::Io)?;
        state
            .entries
            .insert((Layer::Upper, path), entry.ok_or(ResolveHostError::Io)?);
        Ok(())
    }
    fn rollback_mutation(&self, mutation: MutationHandle) {
        let mut state = self.state.lock().unwrap();
        state.staged.remove(&mutation.raw());
        state.transcript.push(format!("rollback:{}", mutation.raw()));
    }
}
impl FakeOverlayHost {
    fn stage_node(&self, mutation: MutationHandle, entry: LayerEntry) -> Result<(), ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        state.transcript.push(format!("stage:{}", mutation.raw()));
        if state.failure == Some(FailurePoint::Stage) {
            return Err(ResolveHostError::Io);
        }
        let staged = state.staged.get_mut(&mutation.raw()).ok_or(ResolveHostError::Io)?;
        staged.1 = Some(entry);
        Ok(())
    }
}
struct Fixture;
impl Fixture {
    fn metadata(size: u64) -> NodeMetadata {
        NodeMetadata {
            permissions: 0o644,
            owner: 11,
            group: 22,
            size,
            modified_seconds: 33,
            modified_nanoseconds: 44,
        }
    }
    fn node(kind: OverlayNodeKind, size: u64, opaque: bool) -> LayerEntry {
        LayerEntry::Node {
            kind,
            metadata: Self::metadata(size),
            opaque,
        }
    }
    fn child(name: &str, kind: OverlayNodeKind, whiteout: bool) -> DirectoryEntry {
        DirectoryEntry {
            name: GuestName::new(name.as_bytes()).unwrap(),
            kind,
            whiteout,
        }
    }
    fn path(value: &str) -> GuestPathBytes {
        GuestPathBytes::new(value.as_bytes()).unwrap()
    }
}

#[test]
fn commit_failure_content() {
    let host = FakeOverlayHost::new();
    host.put(
        Layer::Lower(0),
        "/app",
        Fixture::node(OverlayNodeKind::Regular, 10, false),
    );
    let overlay = Overlay::new(host.clone(), 1).unwrap();
    let plan = overlay.plan_copy_up(&Fixture::path("/app"), false).unwrap();
    host.fail_at(FailurePoint::Commit);
    assert_eq!(overlay.copy_up(&plan), Err(OverlayError::Host(ResolveHostError::Io)));
    assert_eq!(host.published("/app"), LayerEntry::Absent);
}

#[test]
fn invalid_utf8_atomicity() {
    let host = FakeOverlayHost::new();
    let path = b"/dir/\xff";
    host.put(
        Layer::Lower(0),
        "/dir",
        Fixture::node(OverlayNodeKind::Directory, 0, false),
    );
    host.put_bytes(
        Layer::Lower(0),
        path,
        Fixture::node(OverlayNodeKind::Regular, 12, false),
    );
    let overlay = Overlay::new(host.clone(), 1).unwrap();
    let plan = overlay
        .plan_copy_up(&GuestPathBytes::new(path).unwrap(), false)
        .unwrap();
    assert_eq!(plan.path.as_bytes(), path);

    host.fail_at(FailurePoint::Commit);
    assert_eq!(overlay.copy_up(&plan), Err(OverlayError::Host(ResolveHostError::Io)));
    let state = host.state.lock().unwrap();
    assert_eq!(state.entries.get(&(Layer::Upper, path.to_vec())), None);
    assert!(state.staged.is_empty());
}

#[test]
fn invalid_utf8_bytes() {
    let host = FakeOverlayHost::new();
    let directory = b"/\xfe";
    for layer in [Layer::Upper, Layer::Lower(0)] {
        host.put_bytes(layer, directory, Fixture::node(OverlayNodeKind::Directory, 0, false));
    }
    let first = GuestName::new(b"\xff").unwrap();
    let second = GuestName::new(b"\xfe").unwrap();
    {
        let mut state = host.state.lock().unwrap();
        state.directories.insert(
            (Layer::Upper, directory.to_vec()),
            vec![DirectoryEntry {
                name: first.clone(),
                kind: OverlayNodeKind::Regular,
                whiteout: false,
            }],
        );
        state.directories.insert(
            (Layer::Lower(0), directory.to_vec()),
            vec![
                DirectoryEntry {
                    name: first,
                    kind: OverlayNodeKind::Directory,
                    whiteout: false,
                },
                DirectoryEntry {
                    name: second,
                    kind: OverlayNodeKind::Symlink,
                    whiteout: false,
                },
            ],
        );
    }
    let merged = Overlay::new(host, 1)
        .unwrap()
        .read_directory(&GuestPathBytes::new(directory).unwrap())
        .unwrap();
    let names: Vec<&[u8]> = merged.entries.iter().map(|entry| entry.name.as_bytes()).collect();
    assert_eq!(names, vec![b"\xff".as_slice(), b"\xfe".as_slice()]);
}

#[test]
fn byte_path_components() {
    let host = FakeOverlayHost::new();
    host.put_bytes(
        Layer::Upper,
        b"/\xff",
        Fixture::node(OverlayNodeKind::Directory, 0, false),
    );
    host.put_bytes(
        Layer::Upper,
        b"/\xff/leaf",
        Fixture::node(OverlayNodeKind::Regular, 1, false),
    );
    let overlay = Overlay::new(host, 0).unwrap();
    assert!(matches!(
        overlay
            .lookup(&GuestPathBytes::new(b"/skip/../\xff/./leaf").unwrap())
            .unwrap(),
        OverlayLookup::Present {
            layer: Layer::Upper,
            ..
        }
    ));
}
#[test]
fn lookup_observes_ancestors() {
    let host = FakeOverlayHost::new();
    host.put(
        Layer::Upper,
        "/etc",
        Fixture::node(OverlayNodeKind::Directory, 0, false),
    );
    host.put(Layer::Upper, "/etc/hidden", LayerEntry::Whiteout);
    host.put(
        Layer::Lower(0),
        "/etc/hidden",
        Fixture::node(OverlayNodeKind::Regular, 5, false),
    );
    host.put(
        Layer::Upper,
        "/opaque",
        Fixture::node(OverlayNodeKind::Directory, 0, true),
    );
    host.put(
        Layer::Lower(0),
        "/opaque/lower",
        Fixture::node(OverlayNodeKind::Regular, 9, false),
    );
    let upper_winner = Fixture::node(OverlayNodeKind::Regular, 1, false);
    let lower_winner = Fixture::node(OverlayNodeKind::Regular, 2, false);
    host.put(Layer::Upper, "/winner", upper_winner);
    host.put(Layer::Lower(0), "/winner", lower_winner);
    host.put(Layer::Lower(0), "/lower-hidden", LayerEntry::Whiteout);
    let hidden_lower = Fixture::node(OverlayNodeKind::Regular, 3, false);
    host.put(Layer::Lower(1), "/lower-hidden", hidden_lower);
    let overlay = Overlay::new(host, 2).unwrap();
    assert!(matches!(
        overlay.lookup(&Fixture::path("/etc/hidden")).unwrap(),
        OverlayLookup::Absent { .. }
    ));
    assert!(matches!(
        overlay.lookup(&Fixture::path("/opaque/lower")).unwrap(),
        OverlayLookup::Absent { .. }
    ));
    let winner = overlay.lookup(&Fixture::path("/winner")).unwrap();
    assert!(matches!(
        winner,
        OverlayLookup::Present {
            layer: Layer::Upper,
            ..
        }
    ));
    let hidden = overlay.lookup(&Fixture::path("/lower-hidden")).unwrap();
    assert!(matches!(hidden, OverlayLookup::Absent { .. }));
}
#[test]
fn merged_directory_order() {
    let host = FakeOverlayHost::new();
    for layer in [Layer::Upper, Layer::Lower(0), Layer::Lower(1)] {
        host.put(layer, "/dir", Fixture::node(OverlayNodeKind::Directory, 0, false));
    }
    host.directory(
        Layer::Upper,
        "/dir",
        vec![
            Fixture::child("z", OverlayNodeKind::Regular, false),
            Fixture::child("a", OverlayNodeKind::Other, true),
            Fixture::child("common", OverlayNodeKind::Regular, false),
        ],
    );
    host.directory(
        Layer::Lower(0),
        "/dir",
        vec![
            Fixture::child("a", OverlayNodeKind::Regular, false),
            Fixture::child("b", OverlayNodeKind::Directory, false),
            Fixture::child("common", OverlayNodeKind::Symlink, false),
        ],
    );
    host.directory(
        Layer::Lower(1),
        "/dir",
        vec![
            Fixture::child("c", OverlayNodeKind::Regular, false),
            Fixture::child("b", OverlayNodeKind::Regular, false),
        ],
    );
    let overlay = Overlay::new(host, 2).unwrap();
    let merged = overlay.read_directory(&Fixture::path("/dir")).unwrap();
    let names: Vec<&[u8]> = merged.entries.iter().map(|entry| entry.name.as_bytes()).collect();
    assert_eq!(
        names,
        vec![b"z".as_slice(), b"common".as_slice(), b"b".as_slice(), b"c".as_slice(),]
    );
}
#[test]
fn copy_up_publication() {
    let host = FakeOverlayHost::new();
    host.put(
        Layer::Lower(0),
        "/app",
        Fixture::node(OverlayNodeKind::Regular, 91, false),
    );
    let overlay = Overlay::new(host.clone(), 1).unwrap();
    let path = Fixture::path("/app");
    let plan = overlay.plan_copy_up(&path, false).unwrap();
    assert_eq!(plan.content, CopyContent::Regular { size: 91 });
    assert_eq!(overlay.plan_copy_up(&path, true), Err(OverlayError::ReadOnly));
    host.fail_at(FailurePoint::Stage);
    assert_eq!(overlay.copy_up(&plan), Err(OverlayError::Host(ResolveHostError::Io)));
    assert_eq!(host.published("/app"), LayerEntry::Absent);
    let transcript = host.transcript();
    assert!(transcript.iter().any(|event| event.starts_with("rollback:")));
}
#[test]
fn staged_copy_point() {
    let host = FakeOverlayHost::new();
    host.put(
        Layer::Lower(0),
        "/app",
        Fixture::node(OverlayNodeKind::Regular, 10, false),
    );
    let overlay = Arc::new(Overlay::new(host.clone(), 1).unwrap());
    let path = Fixture::path("/app");
    let plan = overlay.plan_copy_up(&path, false).unwrap();
    let gate = host.gate_commit();
    let worker_overlay = overlay.clone();
    let worker = thread::spawn(move || worker_overlay.copy_up(&plan));
    gate.staged.wait();
    assert!(matches!(
        overlay.lookup(&path).unwrap(),
        OverlayLookup::Present {
            layer: Layer::Lower(0),
            ..
        }
    ));
    gate.release.wait();
    worker.join().unwrap().unwrap();
    assert!(matches!(
        overlay.lookup(&path).unwrap(),
        OverlayLookup::Present {
            layer: Layer::Upper,
            ..
        }
    ));
}
#[test]
fn create_respects_read() {
    let host = FakeOverlayHost::new();
    host.put(Layer::Upper, "/", Fixture::node(OverlayNodeKind::Directory, 0, false));
    host.put(
        Layer::Lower(0),
        "/exists",
        Fixture::node(OverlayNodeKind::Regular, 1, false),
    );
    host.put(Layer::Upper, "/new", LayerEntry::Whiteout);
    let overlay = Overlay::new(host.clone(), 1).unwrap();
    let metadata = Fixture::metadata(0);
    assert_eq!(
        overlay.plan_create(
            &Fixture::path("/exists"),
            OverlayNodeKind::Regular,
            metadata.clone(),
            false,
        ),
        Err(OverlayError::AlreadyExists)
    );
    assert_eq!(
        overlay.plan_create(&Fixture::path("/new"), OverlayNodeKind::Regular, metadata.clone(), true,),
        Err(OverlayError::ReadOnly)
    );
    let plan = overlay
        .plan_create(&Fixture::path("/new"), OverlayNodeKind::Regular, metadata, false)
        .unwrap();
    overlay.create(&plan).unwrap();
    assert!(matches!(host.published("/new"), LayerEntry::Node { .. }));
}
