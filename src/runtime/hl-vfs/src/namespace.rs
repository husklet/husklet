use std::sync::RwLock;

use crate::{GuestPath, GuestPathBytes, PathError, ReadOnlyPaths};

const MOUNT_MAXIMUM: usize = 256;
const MOUNT_PATH_CAPACITY: usize = 256;

/// Opaque identity of a host or provider-backed mount source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MountSourceId(u64);

impl MountSourceId {
    pub fn new(value: u64) -> Result<Self, MountError> {
        if value == 0 {
            Err(MountError::InvalidSource)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Append-only namespace slot identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MountId(u16);

impl MountId {
    fn from_index(index: usize) -> Self {
        Self(u16::try_from(index + 1).expect("bounded mount index"))
    }
}

/// Matching behavior of one projected mount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountKind {
    Directory,
    File,
    ProjectedSymlink,
}

/// Failure to mutate a mount namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountError {
    InvalidPath(PathError),
    RelativePath,
    InvalidSource,
    PathTooLong,
    Capacity,
    NotMounted,
}

/// Pointer-free checkpoint representation of one mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountSnapshot {
    pub id: MountId,
    pub guest_path: GuestPath,
    pub source: MountSourceId,
    pub kind: MountKind,
    pub read_only: bool,
    pub active: bool,
}

#[derive(Clone, Debug)]
struct Mount {
    snapshot: MountSnapshot,
}

impl Mount {
    fn matches_bytes(&self, path: &[u8]) -> bool {
        if !self.snapshot.active {
            return false;
        }
        let target = self.snapshot.guest_path.as_str().as_bytes();
        match self.snapshot.kind {
            MountKind::File => path == target,
            MountKind::Directory | MountKind::ProjectedSymlink => {
                path == target || path.strip_prefix(target).is_some_and(|suffix| suffix.starts_with(b"/"))
            }
        }
    }
}

/// Result of routing an absolute guest path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountRoute {
    Root,
    Mounted {
        id: MountId,
        source: MountSourceId,
        kind: MountKind,
        read_only: bool,
    },
}

/// Per-runtime append-only bind-mount routing table.
pub struct MountNamespace {
    mounts: RwLock<Vec<Mount>>,
}

impl MountNamespace {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            mounts: RwLock::new(Vec::new()),
        }
    }

    pub fn mount(
        &self,
        guest_path: &str,
        source: MountSourceId,
        kind: MountKind,
        read_only: bool,
    ) -> Result<MountId, MountError> {
        let guest_path = GuestPath::new(guest_path).map_err(MountError::InvalidPath)?;
        if !guest_path.is_absolute() {
            return Err(MountError::RelativePath);
        }
        if guest_path.as_str().len() >= MOUNT_PATH_CAPACITY {
            return Err(MountError::PathTooLong);
        }
        let mut mounts = self.mounts.write().unwrap_or_else(|error| error.into_inner());
        if mounts.len() == MOUNT_MAXIMUM {
            return Err(MountError::Capacity);
        }
        let id = MountId::from_index(mounts.len());
        mounts.push(Mount {
            snapshot: MountSnapshot {
                id,
                guest_path,
                source,
                kind,
                read_only,
                active: true,
            },
        });
        Ok(id)
    }

    pub fn unmount(&self, guest_path: &GuestPath) -> Result<(), MountError> {
        let mut mounts = self.mounts.write().unwrap_or_else(|error| error.into_inner());
        let mut found = false;
        for mount in mounts.iter_mut() {
            if mount.snapshot.active && mount.snapshot.guest_path == *guest_path {
                mount.snapshot.active = false;
                found = true;
            }
        }
        if found { Ok(()) } else { Err(MountError::NotMounted) }
    }

    #[must_use]
    pub fn route(&self, path: &GuestPath) -> MountRoute {
        self.route_for_bytes(path.as_str().as_bytes())
    }

    /// Routes exact guest pathname bytes against configured mount text.
    ///
    /// Configuration remains UTF-8 text, while guest suffix bytes are never
    /// decoded or normalized.
    #[must_use]
    pub fn route_bytes(&self, path: &GuestPathBytes) -> MountRoute {
        self.route_for_bytes(path.as_bytes())
    }

    fn route_for_bytes(&self, path: &[u8]) -> MountRoute {
        let mounts = self.mounts.read().unwrap_or_else(|error| error.into_inner());
        let mut selected = None;
        let mut selected_length = 0;
        for mount in mounts.iter().filter(|mount| mount.matches_bytes(path)) {
            let length = mount.snapshot.guest_path.as_str().len();
            if length > selected_length {
                selected = Some(mount);
                selected_length = length;
            }
        }
        selected.map_or(MountRoute::Root, |mount| MountRoute::Mounted {
            id: mount.snapshot.id,
            source: mount.snapshot.source,
            kind: mount.snapshot.kind,
            read_only: mount.snapshot.read_only,
        })
    }

    /// Returns the first live mount published at exactly `path`.
    ///
    /// Equal-path publication follows the C `jail_match` contract: the first
    /// slot remains authoritative until it is detached.
    #[must_use]
    pub fn mounted_at(&self, path: &GuestPath) -> Option<MountRoute> {
        self.mounted_at_raw(path.as_str().as_bytes())
    }

    /// Returns the first live mount configured at exactly these guest bytes.
    #[must_use]
    pub fn mounted_at_bytes(&self, path: &GuestPathBytes) -> Option<MountRoute> {
        self.mounted_at_raw(path.as_bytes())
    }

    fn mounted_at_raw(&self, path: &[u8]) -> Option<MountRoute> {
        self.mounts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|mount| mount.snapshot.active && mount.snapshot.guest_path.as_str().as_bytes() == path)
            .map(|mount| MountRoute::Mounted {
                id: mount.snapshot.id,
                source: mount.snapshot.source,
                kind: mount.snapshot.kind,
                read_only: mount.snapshot.read_only,
            })
    }

    /// Applies deepest-mount precedence before root/subtree read-only policy.
    #[must_use]
    pub fn denies_write(&self, path: &GuestPath, root_read_only: bool, subtrees: &ReadOnlyPaths) -> bool {
        match self.route(path) {
            MountRoute::Mounted { read_only, .. } => read_only,
            MountRoute::Root => subtrees.denies(path) || (root_read_only && !Self::writable_pseudo_mount(path)),
        }
    }

    /// Applies mount and configured read-only policy to exact guest bytes.
    #[must_use]
    pub fn denies_write_bytes(&self, path: &GuestPathBytes, root_read_only: bool, subtrees: &ReadOnlyPaths) -> bool {
        match self.route_bytes(path) {
            MountRoute::Mounted { read_only, .. } => read_only,
            MountRoute::Root => {
                subtrees.denies_bytes(path) || (root_read_only && !Self::pseudo_writable(path.as_bytes()))
            }
        }
    }

    fn writable_pseudo_mount(path: &GuestPath) -> bool {
        Self::pseudo_writable(path.as_str().as_bytes())
    }

    fn pseudo_writable(path: &[u8]) -> bool {
        [b"/proc".as_slice(), b"/dev", b"/sys", b"/tmp", b"/run"]
            .iter()
            .any(|root| path == *root || path.strip_prefix(*root).is_some_and(|suffix| suffix.starts_with(b"/")))
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<MountSnapshot> {
        self.mounts
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|mount| mount.snapshot.clone())
            .collect()
    }
}

impl Default for MountNamespace {
    fn default() -> Self {
        Self::new()
    }
}
