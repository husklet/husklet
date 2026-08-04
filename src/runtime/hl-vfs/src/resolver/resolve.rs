use std::collections::VecDeque;

use super::path::{Component, PreparedPath};
use super::pin::PinStack;
use super::{ResolveError, ResolveHostError};
use crate::{GuestName, GuestPathBytes, MountKind, MountNamespace, MountRoute, MountSourceId};

const FOLLOW_MAXIMUM: u32 = 40;
const RESOLUTION_COMPONENT_MAXIMUM: usize = 512;
const RESOLUTION_PATH_MAXIMUM: usize = 4096;

/// Opaque host node pin. Values have meaning only to the selected host adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct NodeHandle(u64);

impl NodeHandle {
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Type observed while inspecting one pinned child without following links.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Directory,
    File,
    Symlink,
    MagicLink,
    Other,
}

/// Capabilities consumed by the pinned VFS component walker.
///
/// Every successful pin is released exactly once through [`VfsHost::close`].
/// Child inspection is relative to an already pinned directory and never
/// follows a symlink.
pub trait VfsHost: Send + Sync {
    /// Independently owned capability suitable for an operation relative to a
    /// successfully resolved parent. Its representation remains host-owned.
    type ParentLease;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError>;

    fn pin_mount(&self, source: MountSourceId) -> Result<NodeHandle, ResolveHostError>;

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError>;

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError>;

    fn crosses_mount(&self, _directory: NodeHandle, _child: NodeHandle) -> Result<bool, ResolveHostError> {
        Ok(false)
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError>;

    fn close(&self, node: NodeHandle);
}

/// Input to a confined component walk.
#[derive(Clone, Copy, Debug)]
pub struct ResolveRequest<'path> {
    pub path: &'path GuestPathBytes,
    pub base: &'path GuestPathBytes,
    pub nofollow_final: bool,
    pub no_symlinks: bool,
    pub allow_missing_final: bool,
}

/// Additional `openat2` traversal constraints enforced during a pinned walk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolveConstraints {
    pub no_cross_device: bool,
    pub no_magic_links: bool,
    pub beneath: bool,
    pub in_root: bool,
}

/// Pinned parent directory and final component returned by a resolution.
///
/// Dropping this value releases the parent pin. The consumer may retain it
/// across a host open operation, eliminating a path check/use gap.
#[derive(Debug)]
pub struct ResolvedParent<'host, H: VfsHost> {
    host: &'host H,
    parent: Option<NodeHandle>,
    final_component: ResolvedComponent,
}

impl<H: VfsHost> ResolvedParent<'_, H> {
    #[must_use]
    pub fn parent(&self) -> NodeHandle {
        self.parent.expect("resolved parent remains owned")
    }

    /// Duplicates the pinned parent without exposing its host representation.
    /// The returned capability owns an independent lifetime.
    pub fn duplicate_parent(&self) -> Result<H::ParentLease, ResolveHostError> {
        self.host.duplicate_parent(self.parent())
    }

    #[must_use]
    pub const fn final_component(&self) -> &ResolvedComponent {
        &self.final_component
    }

    /// Returns the ordinary child name, or `None` when the walk resolved the
    /// root of its current pinned filesystem.
    #[must_use]
    pub const fn final_name(&self) -> Option<&GuestName> {
        self.final_component.as_name()
    }
}

/// Final lookup component retained with its pinned parent.
///
/// A walk ending at the guest root or a mounted directory names the pinned
/// filesystem root as state rather than manufacturing a `.` child name.
/// Ordinary child names retain [`GuestName`]'s byte-level invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedComponent {
    Root,
    Name(GuestName),
}

impl ResolvedComponent {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Root => b"/",
            Self::Name(name) => name.as_bytes(),
        }
    }

    #[must_use]
    pub const fn as_name(&self) -> Option<&GuestName> {
        match self {
            Self::Root => None,
            Self::Name(name) => Some(name),
        }
    }
}

impl<H: VfsHost> Drop for ResolvedParent<'_, H> {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.take() {
            self.host.close(parent);
        }
    }
}

/// Host-neutral, mount-aware pinned component walker.
pub struct Resolver<'namespace, H: VfsHost> {
    host: H,
    namespace: &'namespace MountNamespace,
}

impl<'namespace, H: VfsHost> Resolver<'namespace, H> {
    pub const fn new(host: H, namespace: &'namespace MountNamespace) -> Self {
        Self { host, namespace }
    }

    #[must_use]
    pub const fn host(&self) -> &H {
        &self.host
    }

    pub fn resolve(&self, request: ResolveRequest<'_>) -> Result<ResolvedParent<'_, H>, ResolveError> {
        self.resolve_with(request, ResolveConstraints::default())
    }

    pub fn resolve_with(
        &self,
        request: ResolveRequest<'_>,
        constraints: ResolveConstraints,
    ) -> Result<ResolvedParent<'_, H>, ResolveError> {
        if constraints.beneath && request.path.is_absolute() {
            return Err(ResolveError::Escape);
        }
        let prepared = PreparedPath::new_with_root(request.path, request.base, constraints.in_root)?;
        let mut walk = Walk::new(&self.host, self.namespace, prepared, constraints)?;
        walk.run(request)
    }
}

struct Walk<'host, 'namespace, H: VfsHost> {
    host: &'host H,
    namespace: &'namespace MountNamespace,
    remaining: VecDeque<Component>,
    guest_components: Vec<GuestName>,
    pins: PinStack<'host, H>,
    follows: u32,
    constraints: ResolveConstraints,
    boundary: usize,
}

impl<'host, 'namespace, H: VfsHost> Walk<'host, 'namespace, H> {
    fn new(
        host: &'host H,
        namespace: &'namespace MountNamespace,
        prepared: PreparedPath,
        constraints: ResolveConstraints,
    ) -> Result<Self, ResolveError> {
        let boundary = if constraints.beneath || constraints.in_root {
            prepared.base_depth
        } else {
            0
        };
        Ok(Self {
            host,
            namespace,
            remaining: prepared.components,
            guest_components: Vec::new(),
            pins: PinStack::root(host)?,
            follows: 0,
            constraints,
            boundary,
        })
    }

    fn run(&mut self, request: ResolveRequest<'_>) -> Result<ResolvedParent<'host, H>, ResolveError> {
        loop {
            let Some(component) = self.remaining.pop_front() else {
                return self.finish(ResolvedComponent::Root);
            };
            if let Some(resolved) = self.step(component, request)? {
                return Ok(resolved);
            }
        }
    }

    fn step(
        &mut self,
        component: Component,
        request: ResolveRequest<'_>,
    ) -> Result<Option<ResolvedParent<'host, H>>, ResolveError> {
        let Component::Name(component) = component else {
            if matches!(component, Component::Parent) {
                self.parent()?;
            }
            return Ok(None);
        };
        let last = self.remaining.is_empty();
        if self.enter_mount(&component)? {
            return if last {
                self.finish(ResolvedComponent::Root).map(Some)
            } else {
                Ok(None)
            };
        }
        if last && request.nofollow_final && !request.no_symlinks {
            return self.finish(ResolvedComponent::Name(component)).map(Some);
        }
        self.consume_inspected(component, last, request)
    }

    fn consume_inspected(
        &mut self,
        component: GuestName,
        last: bool,
        request: ResolveRequest<'_>,
    ) -> Result<Option<ResolvedParent<'host, H>>, ResolveError> {
        let inspected = self.inspect(&component, request.allow_missing_final && last)?;
        let Inspected::Node(node, kind) = inspected else {
            return self.finish(ResolvedComponent::Name(component)).map(Some);
        };
        if self.constraints.no_cross_device
            && self
                .host
                .crosses_mount(self.pins.current(), node)
                .map_err(ResolveError::Host)?
        {
            self.host.close(node);
            return Err(ResolveError::CrossDevice);
        }
        if last && !matches!(kind, NodeKind::Symlink | NodeKind::MagicLink) {
            self.host.close(node);
            return self.finish(ResolvedComponent::Name(component)).map(Some);
        }
        self.consume_node(component, node, kind, last, request)?;
        Ok(None)
    }

    fn inspect(&self, component: &GuestName, allow_missing: bool) -> Result<Inspected, ResolveError> {
        match self.host.inspect_child(self.pins.current(), component) {
            Ok((node, kind)) => Ok(Inspected::Node(node, kind)),
            Err(ResolveHostError::NotFound) if allow_missing => Ok(Inspected::Missing),
            Err(error) => Err(ResolveError::Host(error)),
        }
    }

    fn consume_node(
        &mut self,
        component: GuestName,
        node: NodeHandle,
        kind: NodeKind,
        last: bool,
        request: ResolveRequest<'_>,
    ) -> Result<(), ResolveError> {
        if matches!(kind, NodeKind::Symlink | NodeKind::MagicLink) {
            if kind == NodeKind::MagicLink && self.constraints.no_magic_links {
                self.host.close(node);
                return Err(ResolveError::MagicLinkForbidden);
            }
            if request.no_symlinks {
                self.host.close(node);
                return Err(ResolveError::SymlinkForbidden);
            }
            return self.follow_link(node);
        }
        if last {
            return Ok(());
        }
        if kind != NodeKind::Directory {
            self.host.close(node);
            return Err(ResolveError::NotDirectory);
        }
        self.guest_components.push(component);
        self.pins.push(node);
        Ok(())
    }

    fn follow_link(&mut self, node: NodeHandle) -> Result<(), ResolveError> {
        self.follows += 1;
        if self.follows > FOLLOW_MAXIMUM {
            self.host.close(node);
            return Err(ResolveError::SymlinkLoop);
        }
        let mut output = vec![0_u8; RESOLUTION_PATH_MAXIMUM + 1];
        let read = self.host.read_link(node, &mut output);
        self.host.close(node);
        let length = read.map_err(ResolveError::Host)?;
        if length > RESOLUTION_PATH_MAXIMUM {
            return Err(ResolveError::PathTooLong);
        }
        let absolute = output[..length].starts_with(b"/");
        let mut inserted = PreparedPath::from_bytes(&output[..length])?.components;
        inserted.append(&mut self.remaining);
        self.ensure_remaining_bound(&inserted)?;
        self.remaining = inserted;
        if absolute {
            if self.constraints.beneath {
                return Err(ResolveError::Escape);
            }
            if self.constraints.in_root {
                self.guest_components.truncate(self.boundary);
                self.pins.truncate(self.boundary + 1);
            } else {
                self.guest_components.clear();
                self.pins.reset()?;
            }
        }
        Ok(())
    }

    fn ensure_remaining_bound(&self, components: &VecDeque<Component>) -> Result<(), ResolveError> {
        if components.len() > RESOLUTION_COMPONENT_MAXIMUM {
            return Err(ResolveError::TooManyComponents);
        }
        let size = components
            .iter()
            .map(|component| component.byte_length() + 1)
            .sum::<usize>();
        if size > RESOLUTION_PATH_MAXIMUM {
            return Err(ResolveError::PathTooLong);
        }
        Ok(())
    }

    fn parent(&mut self) -> Result<(), ResolveError> {
        if self.guest_components.len() <= self.boundary {
            return if self.constraints.beneath {
                Err(ResolveError::Escape)
            } else {
                Ok(())
            };
        }
        if self.guest_components.pop().is_some() {
            self.pins.pop();
        }
        Ok(())
    }

    fn enter_mount(&mut self, component: &GuestName) -> Result<bool, ResolveError> {
        let candidate = self.candidate_path(component)?;
        let Some(route) = self.namespace.mounted_at_bytes(&candidate) else {
            return Ok(false);
        };
        let MountRoute::Mounted { source, kind, .. } = route else {
            return Ok(false);
        };
        if self.constraints.no_cross_device {
            return Err(ResolveError::CrossDevice);
        }
        if kind != MountKind::Directory {
            return Err(ResolveError::UnsupportedMountKind);
        }
        let pin = self.host.pin_mount(source).map_err(ResolveError::Host)?;
        self.guest_components.push(component.clone());
        self.pins.push(pin);
        Ok(true)
    }

    fn candidate_path(&self, component: &GuestName) -> Result<GuestPathBytes, ResolveError> {
        let length = self
            .guest_components
            .iter()
            .map(|name| name.as_bytes().len() + 1)
            .sum::<usize>()
            .checked_add(component.as_bytes().len() + 1)
            .ok_or(ResolveError::PathTooLong)?;
        let mut path = Vec::with_capacity(length);
        for existing in &self.guest_components {
            path.push(b'/');
            path.extend_from_slice(existing.as_bytes());
        }
        path.push(b'/');
        path.extend_from_slice(component.as_bytes());
        GuestPathBytes::new(&path).map_err(ResolveError::Path)
    }

    fn finish(&mut self, final_component: ResolvedComponent) -> Result<ResolvedParent<'host, H>, ResolveError> {
        let parent = self.pins.take_current();
        Ok(ResolvedParent {
            host: self.host,
            parent: Some(parent),
            final_component,
        })
    }
}

enum Inspected {
    Missing,
    Node(NodeHandle, NodeKind),
}
