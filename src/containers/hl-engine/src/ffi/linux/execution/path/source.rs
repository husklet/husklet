use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_runtime::{GuestPath, MountKind, MountNamespace, MountSourceId, ReadOnlyPaths, RuntimePathError};

use super::{HostError, pin};

#[path = "source_binding.rs"]
mod name_bind;

use name_bind::NameBind;

/// One native mount source and its reverse guest-path projection.
#[derive(Debug)]
pub(super) struct MountPath {
    source: MountSourceId,
    guest: GuestPath,
    host: PathBuf,
    root: Arc<File>,
}

/// Bounded reverse registry shared by the native resolver and path projection.
#[derive(Debug, Default)]
pub(super) struct MountPaths {
    entries: RwLock<Vec<MountPath>>,
}

impl MountPaths {
    fn insert(&self, path: MountPath) {
        self.entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path);
    }

    pub(super) fn root(&self, source: MountSourceId) -> Result<Arc<File>, RuntimePathError> {
        self.entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| entry.source == source)
            .map(|entry| Arc::clone(&entry.root))
            .ok_or(RuntimePathError::NotFound)
    }

    fn guest(&self, path: &Path) -> Result<Option<GuestPath>, RuntimePathError> {
        let entries = self.entries.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let selected = entries
            .iter()
            .filter_map(|entry| path.strip_prefix(&entry.host).ok().map(|relative| (entry, relative)))
            .max_by_key(|(entry, _)| entry.host.components().count());
        selected
            .map(|(entry, relative)| Self::join(&entry.guest, relative))
            .transpose()
    }

    fn join(guest: &GuestPath, relative: &Path) -> Result<GuestPath, RuntimePathError> {
        let relative = relative.to_str().ok_or(RuntimePathError::Invalid)?;
        let mut projected = guest.as_str().trim_end_matches('/').to_owned();
        if !relative.is_empty() {
            projected.push('/');
            projected.push_str(relative);
        } else if projected.is_empty() && guest.is_absolute() {
            projected.push('/');
        }
        GuestPath::new(&projected).map_err(|_| RuntimePathError::Invalid)
    }
}

/// Loads the name index a committed image chain publishes beside its tree.
///
/// The sidecar lives at `<store>/index/committed/<chain-id>.idx` next to the
/// `<store>/committed/<chain-id>` tree it enumerates, and is only consulted for
/// a lower whose layout matches that shape. Any missing, oversized, or
/// digest-mismatched sidecar yields `None` and the resolver probes live.
fn layer_index(lower: &Path) -> Option<Arc<hl_fs::LayerIndex>> {
    let name = lower.file_name()?;
    let committed = lower.parent()?;
    if committed.file_name()? != "committed" {
        return None;
    }
    let sidecar = committed
        .parent()?
        .join("index/committed")
        .join(format!("{}.idx", name.to_str()?));
    hl_fs::LayerIndex::load(&sidecar).ok().map(Arc::new)
}

/// Native root, resolver, mount table, and reverse mount projection for one engine.
pub(crate) struct OrdinaryContext {
    root: PathBuf,
    layer_roots: Vec<PathBuf>,
    host: pin::Host,
    mounts: Arc<MountNamespace>,
    paths: Arc<MountPaths>,
    shm_path: PathBuf,
    shm_budget: Arc<super::tmpfs::Budget>,
    tmp_path: PathBuf,
    tmp_budget: Arc<super::tmpfs::Budget>,
    read_only: ReadOnlyPaths,
    root_read_only: AtomicBool,
    name_binds: RwLock<Vec<NameBind>>,
    next_mount_source: AtomicU64,
}

impl OrdinaryContext {
    pub(super) fn new(root: &[u8]) -> Result<Self, RuntimePathError> {
        Self::with_lowers(root, &[])
    }

    pub(super) fn layered(upper: &[u8], lowers: &[Vec<u8>]) -> Result<Self, RuntimePathError> {
        Self::with_lowers(upper, lowers)
    }

    fn with_lowers(root: &[u8], lowers: &[Vec<u8>]) -> Result<Self, RuntimePathError> {
        let root = PathBuf::from(std::ffi::OsStr::from_bytes(root));
        let root = root.canonicalize().map_err(HostError::map)?;
        let root_pin = Arc::new(File::open(&root).map_err(HostError::map)?);
        let lower_roots = lowers
            .iter()
            .map(|lower| {
                let lower = PathBuf::from(std::ffi::OsStr::from_bytes(lower));
                let lower = lower.canonicalize().map_err(HostError::map)?;
                if !lower.is_dir() {
                    return Err(RuntimePathError::NotDirectory);
                }
                let pin = File::open(&lower).map(Arc::new).map_err(HostError::map)?;
                let index = layer_index(&lower);
                Ok((lower, pin, index))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let shared = super::tmpfs::PosixShm::create(&root, &root_pin)?;
        let shm_path = shared.path().to_owned();
        let shm_budget = shared.budget();
        super::tmpfs::PosixShm::create_tmp(&root_pin)?;
        super::tmpfs::PosixShm::create_devpts(&root_pin)?;
        let tmp_path = root.join("tmp");
        let tmp_budget = super::tmpfs::Budget::ordinary();
        let paths = Arc::new(MountPaths::default());
        let mounts = Arc::new(MountNamespace::new());
        mounts
            .mount("/dev/shm", shared.source(), MountKind::Directory, false)
            .map_err(|_| RuntimePathError::Invalid)?;
        paths.insert(MountPath {
            source: shared.source(),
            guest: GuestPath::new("/dev/shm").map_err(|_| RuntimePathError::Invalid)?,
            host: shared.path().to_owned(),
            root: shared.root(),
        });
        let host = if lower_roots.is_empty() {
            pin::Host::new(Arc::clone(&root_pin), Arc::clone(&paths))
        } else {
            let mut roots = Vec::with_capacity(lower_roots.len() + 1);
            let mut indexes = Vec::with_capacity(lower_roots.len() + 1);
            roots.push(Arc::clone(&root_pin));
            indexes.push(None);
            for (_, pin, index) in &lower_roots {
                roots.push(Arc::clone(pin));
                indexes.push(index.clone());
            }
            pin::Host::indexed(roots, indexes, Arc::clone(&paths))
        };
        let mut layer_roots = Vec::with_capacity(lower_roots.len() + 1);
        layer_roots.push(root.clone());
        layer_roots.extend(lower_roots.into_iter().map(|(path, _, _)| path));
        Ok(Self {
            root,
            layer_roots,
            host,
            mounts,
            paths,
            shm_path,
            shm_budget,
            tmp_path,
            tmp_budget,
            read_only: ReadOnlyPaths::new(),
            root_read_only: AtomicBool::new(false),
            name_binds: RwLock::new(Vec::new()),
            next_mount_source: AtomicU64::new(0x1000_0000),
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn host(&self) -> pin::Host {
        self.host.clone()
    }

    pub(super) fn mounts(&self) -> &MountNamespace {
        &self.mounts
    }

    fn mount_port(&self) -> Arc<dyn hl_runtime::ProcfsMountPort> {
        let mounts: Arc<dyn hl_runtime::ProcfsMountPort> = self.mounts.clone();
        mounts
    }

    pub(super) const fn read_only(&self) -> &ReadOnlyPaths {
        &self.read_only
    }

    pub(super) fn shm_budget(&self, path: &Path) -> Option<Arc<super::tmpfs::Budget>> {
        if path.starts_with(&self.shm_path) {
            Some(Arc::clone(&self.shm_budget))
        } else if path.starts_with(&self.tmp_path) {
            Some(Arc::clone(&self.tmp_budget))
        } else {
            None
        }
    }

    pub(super) fn root_read_only(&self) -> bool {
        self.root_read_only.load(Ordering::Acquire)
    }

    pub(super) fn set_root_policy(&self, enabled: bool) {
        self.root_read_only.store(enabled, Ordering::Release);
    }

    pub(in crate::ffi::linux::execution) fn mount_directory(
        &self,
        guest: &str,
        host: &str,
        read_only: bool,
    ) -> Result<(), RuntimePathError> {
        let host = PathBuf::from(host).canonicalize().map_err(HostError::map)?;
        if !host.is_dir() {
            return Ok(());
        }
        let root = Arc::new(File::open(&host).map_err(HostError::map)?);
        let source = MountSourceId::new(self.next_mount_source.fetch_add(1, Ordering::Relaxed))
            .map_err(|_| RuntimePathError::Invalid)?;
        let guest = GuestPath::new(guest).map_err(|_| RuntimePathError::Invalid)?;
        self.mounts
            .mount(guest.as_str(), source, MountKind::Directory, read_only)
            .map_err(|_| RuntimePathError::Invalid)?;
        self.paths.insert(MountPath {
            source,
            guest,
            host,
            root,
        });
        Ok(())
    }

    pub(super) fn guest_path(&self, path: &Path) -> Result<GuestPath, RuntimePathError> {
        if let Some(guest) = self.paths.guest(path)? {
            return Ok(guest);
        }
        let (_, relative) = self
            .layer_roots
            .iter()
            .filter_map(|root| path.strip_prefix(root).ok().map(|relative| (root, relative)))
            .max_by_key(|(root, _)| root.components().count())
            .ok_or(RuntimePathError::Access)?;
        MountPaths::join(&GuestPath::new("/").map_err(|_| RuntimePathError::Invalid)?, relative)
    }
}

/// Deliberately narrow projected namespace capability.
///
/// The current authority grants only one logical executable. Every operation
/// requiring native traversal rejects this source before issuing a host call.
#[derive(Clone)]
pub(super) struct ProjectedContext {
    root: GuestPath,
    tree: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
}

impl ProjectedContext {
    fn new(root: &[u8]) -> Result<Self, RuntimePathError> {
        let root = std::str::from_utf8(root).map_err(|_| RuntimePathError::Invalid)?;
        let root = GuestPath::new(root).map_err(|_| RuntimePathError::Invalid)?;
        if !root.is_absolute() {
            return Err(RuntimePathError::Invalid);
        }
        Ok(Self { root, tree: None })
    }

    pub(super) const fn root(&self) -> &GuestPath {
        &self.root
    }

    pub(super) fn tree(&self) -> Result<&Arc<Mutex<crate::native::AuthorityWorker>>, RuntimePathError> {
        self.tree.as_ref().ok_or(RuntimePathError::Unsupported)
    }

    fn with_tree(mut self, tree: Arc<Mutex<crate::native::AuthorityWorker>>) -> Self {
        self.tree = Some(tree);
        self
    }
}

#[derive(Clone)]
pub(super) enum Source {
    Ordinary(Arc<OrdinaryContext>),
    Projected(ProjectedContext),
}

impl Source {
    pub(super) fn ordinary(root: &[u8]) -> Result<Self, RuntimePathError> {
        OrdinaryContext::new(root).map(Arc::new).map(Self::Ordinary)
    }

    pub(super) fn projected(root: &[u8]) -> Result<Self, RuntimePathError> {
        ProjectedContext::new(root).map(Self::Projected)
    }

    pub(super) fn layered(upper: &[u8], lowers: &[Vec<u8>]) -> Result<Self, RuntimePathError> {
        OrdinaryContext::layered(upper, lowers)
            .map(Arc::new)
            .map(Self::Ordinary)
    }

    pub(super) fn native(&self) -> Result<&OrdinaryContext, RuntimePathError> {
        match self {
            Self::Ordinary(context) => Ok(context),
            Self::Projected(_) => Err(RuntimePathError::Unsupported),
        }
    }

    pub(super) fn mount_port(&self) -> Option<Arc<dyn hl_runtime::ProcfsMountPort>> {
        match self {
            Self::Ordinary(context) => Some(context.mount_port()),
            Self::Projected(_) => None,
        }
    }

    pub(super) const fn is_projected(&self) -> bool {
        self.projected_context().is_some()
    }

    pub(super) const fn projected_context(&self) -> Option<&ProjectedContext> {
        match self {
            Self::Ordinary(_) => None,
            Self::Projected(context) => Some(context),
        }
    }

    pub(super) fn with_tree(self, tree: Arc<Mutex<crate::native::AuthorityWorker>>) -> Result<Self, RuntimePathError> {
        match self {
            Self::Projected(context) => Ok(Self::Projected(context.with_tree(tree))),
            Self::Ordinary(_) => Err(RuntimePathError::Invalid),
        }
    }
}
