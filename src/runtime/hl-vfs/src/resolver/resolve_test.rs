use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::{
    GuestName, GuestPathBytes, MountKind, MountNamespace, MountSourceId, NodeHandle, NodeKind, ResolveConstraints,
    ResolveError, ResolveHostError, ResolveRequest, Resolver, VfsHost,
};

#[derive(Clone, Debug)]
struct FakeHost {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Debug)]
struct FakeNode {
    kind: NodeKind,
    children: BTreeMap<Vec<u8>, u64>,
    target: Vec<u8>,
}

#[derive(Debug)]
struct Swap {
    directory: u64,
    name: Vec<u8>,
    replacement: u64,
}

#[derive(Debug)]
struct FakeState {
    nodes: Vec<FakeNode>,
    mounts: HashMap<u64, u64>,
    pins: HashMap<u64, u64>,
    next_pin: u64,
    swap: Option<Swap>,
    transcript: Vec<String>,
}

impl FakeHost {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                nodes: vec![FakeNode {
                    kind: NodeKind::Directory,
                    children: BTreeMap::new(),
                    target: Vec::new(),
                }],
                mounts: HashMap::new(),
                pins: HashMap::new(),
                next_pin: 1,
                swap: None,
                transcript: Vec::new(),
            })),
        }
    }

    fn add_directory(&self, parent: u64, name: &str) -> u64 {
        self.add_node(parent, name.as_bytes(), NodeKind::Directory, b"")
    }

    fn add_file(&self, parent: u64, name: &str) -> u64 {
        self.add_node(parent, name.as_bytes(), NodeKind::File, b"")
    }

    fn add_link(&self, parent: u64, name: &str, target: &str) -> u64 {
        self.add_node(parent, name.as_bytes(), NodeKind::Symlink, target.as_bytes())
    }

    fn add_link_bytes(&self, parent: u64, name: &[u8], target: &[u8]) -> u64 {
        self.add_node(parent, name, NodeKind::Symlink, target)
    }

    fn add_node(&self, parent: u64, name: &[u8], kind: NodeKind, target: &[u8]) -> u64 {
        let mut state = self.state.lock().unwrap();
        let identity = state.nodes.len() as u64;
        state.nodes.push(FakeNode {
            kind,
            children: BTreeMap::new(),
            target: target.to_vec(),
        });
        state.nodes[parent as usize].children.insert(name.to_vec(), identity);
        identity
    }

    fn bind_mount(&self, source: MountSourceId, node: u64) {
        self.state.lock().unwrap().mounts.insert(source.get(), node);
    }

    fn swap_after_inspect(&self, directory: u64, name: &str, replacement: u64) {
        self.state.lock().unwrap().swap = Some(Swap {
            directory,
            name: name.as_bytes().to_vec(),
            replacement,
        });
    }

    fn pinned_node(&self, handle: NodeHandle) -> u64 {
        self.state.lock().unwrap().pins[&handle.raw()]
    }

    fn live_pins(&self) -> usize {
        self.state.lock().unwrap().pins.len()
    }

    fn transcript(&self) -> Vec<String> {
        self.state.lock().unwrap().transcript.clone()
    }

    fn pin(state: &mut FakeState, node: u64, event: String) -> NodeHandle {
        let pin = state.next_pin;
        state.next_pin += 1;
        state.pins.insert(pin, node);
        state.transcript.push(event);
        NodeHandle::from_raw(pin)
    }

    fn node_for_pin(state: &FakeState, handle: NodeHandle) -> Result<u64, ResolveHostError> {
        state.pins.get(&handle.raw()).copied().ok_or(ResolveHostError::Io)
    }
}

impl VfsHost for FakeHost {
    type ParentLease = NodeHandle;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        Ok(Self::pin(&mut state, 0, "pin-root".to_owned()))
    }

    fn pin_mount(&self, source: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let node = state
            .mounts
            .get(&source.get())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        Ok(Self::pin(&mut state, node, format!("pin-mount:{}", source.get())))
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let directory_node = Self::node_for_pin(&state, directory)?;
        let child = state.nodes[directory_node as usize]
            .children
            .get(component.as_bytes())
            .copied()
            .ok_or(ResolveHostError::NotFound)?;
        let kind = state.nodes[child as usize].kind;
        let pin = Self::pin(
            &mut state,
            child,
            format!(
                "inspect:{directory_node}:{}:{child}",
                String::from_utf8_lossy(component.as_bytes())
            ),
        );
        let should_swap = state
            .swap
            .as_ref()
            .is_some_and(|swap| swap.directory == directory_node && swap.name.as_slice() == component.as_bytes());
        if should_swap {
            let swap = state.swap.take().unwrap();
            state.nodes[directory_node as usize]
                .children
                .insert(swap.name, swap.replacement);
            state.transcript.push("swap".to_owned());
        }
        Ok((pin, kind))
    }

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let node = Self::node_for_pin(&state, link)?;
        let target = state.nodes[node as usize].target.clone();
        if target.len() > output.len() {
            return Err(ResolveHostError::ResourceLimit);
        }
        output[..target.len()].copy_from_slice(&target);
        state.transcript.push(format!("readlink:{node}"));
        Ok(target.len())
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        let mut state = self.state.lock().unwrap();
        let node = Self::node_for_pin(&state, parent)?;
        Ok(Self::pin(&mut state, node, format!("duplicate:{node}")))
    }

    fn close(&self, node: NodeHandle) {
        let mut state = self.state.lock().unwrap();
        let removed = state.pins.remove(&node.raw());
        assert!(removed.is_some(), "pin closed exactly once");
        state.transcript.push(format!("close:{}", node.raw()));
    }
}

struct ResolveFixture;

impl ResolveFixture {
    fn request<'path>(path: &'path str, base: &'path GuestPathBytes) -> ResolveRequest<'path> {
        let path = Box::leak(Box::new(GuestPathBytes::new(path.as_bytes()).unwrap()));
        ResolveRequest {
            path,
            base,
            nofollow_final: false,
            no_symlinks: false,
            allow_missing_final: false,
        }
    }
}

#[test]
fn invalid_byte_expansion() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    let invalid = host.add_node(area, b"\xff", NodeKind::Directory, b"");
    host.add_file(invalid, "leaf");
    host.add_link_bytes(area, b"link", b"\xff");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let path = GuestPathBytes::new(b"/area/link/leaf").unwrap();
    let request = ResolveRequest {
        path: &path,
        base: &root,
        nofollow_final: false,
        no_symlinks: false,
        allow_missing_final: false,
    };
    let resolved = resolver.resolve(request).unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), invalid);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"leaf");
}

#[test]
fn lease_lifetime() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    host.add_file(area, "leaf");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let path = GuestPathBytes::new(b"/area/leaf").unwrap();
    let resolved = resolver
        .resolve(ResolveRequest {
            path: &path,
            base: &root,
            nofollow_final: false,
            no_symlinks: false,
            allow_missing_final: false,
        })
        .unwrap();
    let lease = resolved.duplicate_parent().unwrap();
    assert_eq!(host.pinned_node(lease), area);
    assert_eq!(host.live_pins(), 2);

    drop(resolved);
    assert_eq!(host.live_pins(), 1);
    host.close(lease);
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn byte_component_max() {
    let host = FakeHost::new();
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host, &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let accepted = GuestPathBytes::new(&vec![b'x'; 255]).unwrap();
    let rejected = GuestPathBytes::new(&vec![b'x'; 256]).unwrap();
    let mut request = ResolveRequest {
        path: &accepted,
        base: &root,
        nofollow_final: false,
        no_symlinks: false,
        allow_missing_final: true,
    };
    let resolved = resolver.resolve(request).unwrap();
    assert_eq!(resolved.final_name().unwrap().as_bytes().len(), 255);
    drop(resolved);
    request.path = &rejected;
    assert_eq!(resolver.resolve(request).unwrap_err(), ResolveError::ComponentTooLong);
}

#[test]
fn byte_dot_names() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    let child = host.add_directory(area, "child");
    host.add_file(area, "leaf");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let path = GuestPathBytes::new(b"/area/./child/../leaf").unwrap();
    let request = ResolveRequest {
        path: &path,
        base: &root,
        nofollow_final: false,
        no_symlinks: false,
        allow_missing_final: false,
    };
    let resolved = resolver.resolve(request).unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), area);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"leaf");
    assert_ne!(child, area);
}

#[test]
fn root_clamps_base() {
    let host = FakeHost::new();
    let safe = host.add_directory(0, "safe");
    let base_node = host.add_directory(safe, "base");
    host.add_file(base_node, "target");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let base = GuestPathBytes::new(b"/safe/base").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("../../../../safe/base/target", &base))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), base_node);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"target");
    drop(resolved);
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn relative_link_link() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    host.add_directory(area, "inside");
    host.add_file(area, "target");
    host.add_link(area, "link", "inside");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("/area/link/../target", &root))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), area);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"target");
}

#[test]
fn absolute_link_root() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    host.add_link(area, "link", "/outside");
    let outside = host.add_directory(0, "outside");
    host.add_file(outside, "target");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("/area/link/target", &root))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), outside);
}

#[test]
fn symlink_loop_pin() {
    let host = FakeHost::new();
    host.add_link(0, "loop", "/loop");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    assert_eq!(
        resolver.resolve(ResolveFixture::request("/loop", &root)).unwrap_err(),
        ResolveError::SymlinkLoop
    );
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn absolute_chain_restart() {
    let host = FakeHost::new();
    let run = host.add_directory(0, "run");
    host.add_file(run, "value");
    host.add_link(0, "absolute", "/run");
    host.add_link(0, "chain", "/absolute");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("/chain/../chain/value", &root))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), run);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"value");
}

#[test]
fn nested_chain_restart() {
    let host = FakeHost::new();
    let tmp = host.add_directory(0, "tmp");
    let base = host.add_directory(tmp, "base");
    let run = host.add_directory(base, "run");
    host.add_file(run, "value");
    host.add_link(base, "absolute", "/tmp/base/run");
    host.add_link(base, "chain", "/tmp/base/absolute");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("/tmp/base/chain/value", &root))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), run);
}

#[test]
fn final_nofollow_distinct() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    host.add_link(area, "link", "/target");
    host.add_file(0, "target");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let mut request = ResolveFixture::request("/area/link", &root);
    request.nofollow_final = true;

    let resolved = resolver.resolve(request).unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), area);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"link");
    drop(resolved);

    request.no_symlinks = true;
    assert_eq!(resolver.resolve(request).unwrap_err(), ResolveError::SymlinkForbidden);
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn allow_missing_parent() {
    let host = FakeHost::new();
    let area = host.add_directory(0, "area");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let mut request = ResolveFixture::request("/area/new", &root);
    request.allow_missing_final = true;

    let resolved = resolver.resolve(request).unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), area);
    assert_eq!(resolved.final_name().unwrap().as_bytes(), b"new");
}

#[test]
fn nested_mounts_namespace() {
    let host = FakeHost::new();
    let outer = host.add_directory(0, "outer");
    host.add_file(outer, "sibling");
    let first_root = host.add_directory(0, "first-root");
    let second_root = host.add_directory(0, "second-root");
    host.add_file(second_root, "leaf");
    let first_source = MountSourceId::new(1).unwrap();
    let second_source = MountSourceId::new(2).unwrap();
    host.bind_mount(first_source, first_root);
    host.bind_mount(second_source, second_root);
    let namespace = MountNamespace::new();
    namespace
        .mount("/outer/m", first_source, MountKind::Directory, false)
        .unwrap();
    namespace
        .mount("/outer/m/n", second_source, MountKind::Directory, false)
        .unwrap();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let crossed = resolver
        .resolve(ResolveFixture::request("/outer/m/../sibling", &root))
        .unwrap();
    assert_eq!(host.pinned_node(crossed.parent()), outer);
    drop(crossed);
    let nested = resolver
        .resolve(ResolveFixture::request("/outer/m/n/leaf", &root))
        .unwrap();
    assert_eq!(host.pinned_node(nested.parent()), second_root);
}

#[test]
fn no_xdev_mount() {
    let host = FakeHost::new();
    let source_root = host.add_directory(0, "source");
    let source = MountSourceId::new(1).unwrap();
    host.bind_mount(source, source_root);
    let namespace = MountNamespace::new();
    namespace.mount("/mount", source, MountKind::Directory, false).unwrap();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let constraints = ResolveConstraints {
        no_cross_device: true,
        ..ResolveConstraints::default()
    };

    assert_eq!(
        resolver
            .resolve_with(ResolveFixture::request("/mount", &root), constraints)
            .unwrap_err(),
        ResolveError::CrossDevice,
    );
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn no_magic_link() {
    let host = FakeHost::new();
    host.add_node(0, b"magic", NodeKind::MagicLink, b"/target");
    host.add_file(0, "target");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();
    let constraints = ResolveConstraints {
        no_magic_links: true,
        ..ResolveConstraints::default()
    };

    assert_eq!(
        resolver
            .resolve_with(ResolveFixture::request("/magic", &root), constraints)
            .unwrap_err(),
        ResolveError::MagicLinkForbidden,
    );
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn beneath_blocks_parent() {
    let host = FakeHost::new();
    let safe = host.add_directory(0, "safe");
    host.add_directory(safe, "base");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let base = GuestPathBytes::new(b"/safe/base").unwrap();
    let constraints = ResolveConstraints {
        beneath: true,
        ..ResolveConstraints::default()
    };

    assert_eq!(
        resolver
            .resolve_with(ResolveFixture::request("../escape", &base), constraints)
            .unwrap_err(),
        ResolveError::Escape,
    );
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn in_root_absolute() {
    let host = FakeHost::new();
    let safe = host.add_directory(0, "safe");
    let base_node = host.add_directory(safe, "base");
    host.add_file(base_node, "target");
    host.add_link(base_node, "link", "/target");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let base = GuestPathBytes::new(b"/safe/base").unwrap();
    let constraints = ResolveConstraints {
        in_root: true,
        ..ResolveConstraints::default()
    };

    let direct = resolver
        .resolve_with(ResolveFixture::request("/target", &base), constraints)
        .unwrap();
    assert_eq!(host.pinned_node(direct.parent()), base_node);
    drop(direct);
    let linked = resolver
        .resolve_with(ResolveFixture::request("/link", &base), constraints)
        .unwrap();
    assert_eq!(host.pinned_node(linked.parent()), base_node);
    drop(linked);
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn symlink_swap_link() {
    let host = FakeHost::new();
    let safe = host.add_directory(0, "safe");
    let inside = host.add_directory(safe, "inside");
    host.add_file(inside, "target");
    let original = host.add_link(safe, "link", "inside");
    let replacement = host.add_link(safe, "replacement", "/escape");
    host.swap_after_inspect(safe, "link", replacement);
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    let resolved = resolver
        .resolve(ResolveFixture::request("/safe/link/target", &root))
        .unwrap();
    assert_eq!(host.pinned_node(resolved.parent()), inside);
    let transcript = host.transcript();
    let swap = transcript.iter().position(|event| event == "swap").unwrap();
    let read = transcript
        .iter()
        .position(|event| event == &format!("readlink:{original}"))
        .unwrap();
    assert!(swap < read);
    drop(resolved);
    assert_eq!(
        resolver
            .resolve(ResolveFixture::request("/safe/link/target", &root))
            .unwrap_err(),
        ResolveError::Host(ResolveHostError::NotFound)
    );
    assert_eq!(host.live_pins(), 0);
}

#[test]
fn intermediate_non_pins() {
    let host = FakeHost::new();
    host.add_file(0, "file");
    let namespace = MountNamespace::new();
    let resolver = Resolver::new(host.clone(), &namespace);
    let root = GuestPathBytes::new(b"/").unwrap();

    assert_eq!(
        resolver
            .resolve(ResolveFixture::request("/file/child", &root))
            .unwrap_err(),
        ResolveError::NotDirectory
    );
    assert_eq!(host.live_pins(), 0);
}
