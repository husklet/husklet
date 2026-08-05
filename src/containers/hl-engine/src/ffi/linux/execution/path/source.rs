use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use hl_runtime::{
    GuestName, GuestPath, GuestPathBytes, MountKind, MountNamespace, MountRoute, MountSourceId, ReadOnlyPaths,
    ResolveRequest, Resolver, RuntimePathError,
};

use super::{HostError, pin};

/// One native mount source and its reverse guest-path projection.
#[derive(Debug)]
pub(super) struct MountPath {
    source: MountSourceId,
    guest: GuestPath,
    host: PathBuf,
    root: Arc<File>,
}

const NAME_BIND_MAXIMUM: usize = 32;
const NAME_ALIAS_MAXIMUM: usize = 8;

#[derive(Debug)]
struct NameBind {
    aliases: Vec<GuestName>,
    exact: Vec<GuestPath>,
    read_only: bool,
    host: PathBuf,
    parent: Arc<File>,
    leaf: CString,
}

/// One resolved live alias. The pinned host parent preserves the C oracle's
/// rename-safe lookup identity while each open still observes the current leaf.
#[derive(Clone, Debug)]
pub(super) struct NameBinding {
    pub(super) guest: GuestPath,
    pub(super) host: PathBuf,
    pub(super) parent: Arc<File>,
    pub(super) leaf: CString,
    pub(super) read_only: bool,
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
            .unwrap_or_else(|error| error.into_inner())
            .push(path);
    }

    pub(super) fn root(&self, source: MountSourceId) -> Result<Arc<File>, RuntimePathError> {
        self.entries
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|entry| entry.source == source)
            .map(|entry| Arc::clone(&entry.root))
            .ok_or(RuntimePathError::NotFound)
    }

    fn guest(&self, path: &Path) -> Result<Option<GuestPath>, RuntimePathError> {
        let entries = self.entries.read().unwrap_or_else(|error| error.into_inner());
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
                Ok((lower, pin))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let shared = super::tmpfs::PosixShm::create(&root, &root_pin)?;
        let shm_path = shared.path().to_owned();
        let shm_budget = shared.budget();
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
            roots.push(Arc::clone(&root_pin));
            roots.extend(lower_roots.iter().map(|(_, pin)| Arc::clone(pin)));
            pin::Host::layered(roots, Arc::clone(&paths))
        };
        let mut layer_roots = Vec::with_capacity(lower_roots.len() + 1);
        layer_roots.push(root.clone());
        layer_roots.extend(lower_roots.into_iter().map(|(path, _)| path));
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

    pub(in crate::ffi::linux::execution) fn add_name_binds(&self, spec: &str) -> Result<(), RuntimePathError> {
        if spec.is_empty() {
            return Ok(());
        }
        let mut parsed = Vec::new();
        for record in spec.split('\n') {
            if parsed.len() == NAME_BIND_MAXIMUM {
                return Err(RuntimePathError::TooLarge);
            }
            let mut fields = record.split('\t');
            let host = fields
                .next()
                .filter(|value| !value.is_empty())
                .ok_or(RuntimePathError::Invalid)?;
            let (host, read_only) = host
                .strip_prefix("rw:")
                .map_or_else(|| (host.strip_prefix("ro:").unwrap_or(host), true), |host| (host, false));
            let host = PathBuf::from(host).canonicalize().map_err(HostError::map)?;
            if !host.is_file() {
                return Err(RuntimePathError::Invalid);
            }
            let parent_path = host.parent().ok_or(RuntimePathError::Invalid)?;
            let parent = Arc::new(File::open(parent_path).map_err(HostError::map)?);
            let leaf = host
                .file_name()
                .ok_or(RuntimePathError::Invalid)
                .and_then(|name| CString::new(name.as_bytes()).map_err(|_| RuntimePathError::Invalid))?;
            let mut aliases = Vec::new();
            let mut exact = Vec::new();
            for field in fields {
                if aliases.len() + exact.len() == NAME_ALIAS_MAXIMUM {
                    return Err(RuntimePathError::TooLarge);
                }
                if field.starts_with('/') {
                    let path = GuestPath::new(field).map_err(|_| RuntimePathError::Invalid)?;
                    if exact.contains(&path) {
                        return Err(RuntimePathError::Invalid);
                    }
                    exact.push(path);
                    continue;
                }
                let alias = GuestName::new(field.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
                if aliases.contains(&alias) {
                    return Err(RuntimePathError::Invalid);
                }
                aliases.push(alias);
            }
            if aliases.is_empty() && exact.is_empty() {
                return Err(RuntimePathError::Invalid);
            }
            parsed.push(NameBind {
                aliases,
                exact,
                read_only,
                host,
                parent,
                leaf,
            });
        }
        *self.name_binds.write().unwrap_or_else(|error| error.into_inner()) = parsed;
        Ok(())
    }

    pub(super) fn name_binding(&self, base: &GuestPath, raw: &[u8]) -> Result<Option<NameBinding>, RuntimePathError> {
        let guest = Self::absolute_guest(base, raw)?;
        let guest_bytes = GuestPathBytes::new(guest.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        if !matches!(self.mounts.route_bytes(&guest_bytes), MountRoute::Root) {
            return Ok(None);
        }
        let leaf = guest_bytes
            .as_bytes()
            .rsplit(|byte| *byte == b'/')
            .next()
            .ok_or(RuntimePathError::Invalid)?;
        let parent = guest
            .as_str()
            .rfind('/')
            .map(|slash| if slash == 0 { "/" } else { &guest.as_str()[..slash] })
            .ok_or(RuntimePathError::Invalid)?;
        let rules = self.name_binds.read().unwrap_or_else(|error| error.into_inner());
        if let Some(rule) = rules.iter().find(|rule| rule.exact.contains(&guest)) {
            return Ok(Some(NameBinding {
                guest,
                host: rule.host.clone(),
                parent: Arc::clone(&rule.parent),
                leaf: rule.leaf.clone(),
                read_only: rule.read_only,
            }));
        }
        for rule in rules
            .iter()
            .filter(|rule| rule.aliases.iter().any(|alias| alias.as_bytes() == leaf))
        {
            for alias in &rule.aliases {
                let sibling = if parent == "/" {
                    format!("/{}", String::from_utf8_lossy(alias.as_bytes()))
                } else {
                    format!("{parent}/{}", String::from_utf8_lossy(alias.as_bytes()))
                };
                let sibling = GuestPathBytes::new(sibling.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
                if self.raw_non_directory(&sibling)? {
                    return Ok(Some(NameBinding {
                        guest,
                        host: rule.host.clone(),
                        parent: Arc::clone(&rule.parent),
                        leaf: rule.leaf.clone(),
                        read_only: rule.read_only,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn raw_non_directory(&self, guest: &GuestPathBytes) -> Result<bool, RuntimePathError> {
        let resolver = Resolver::new(self.host(), self.mounts());
        let root = GuestPathBytes::new(b"/").map_err(|_| RuntimePathError::Invalid)?;
        let resolved = match resolver.resolve(ResolveRequest {
            path: guest,
            base: &root,
            nofollow_final: true,
            no_symlinks: false,
            allow_missing_final: false,
        }) {
            Ok(resolved) => resolved,
            Err(_) => return Ok(false),
        };
        let Some(name) = resolved.final_name() else {
            return Ok(false);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let parent = resolved.duplicate_parent().map_err(|_| RuntimePathError::Invalid)?;
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the duplicated parent is live, the name is terminated,
        // and fstatat initializes status on success without retaining pointers.
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Ok(false);
        }
        // SAFETY: successful fstatat initialized the status object.
        Ok(unsafe { status.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR)
    }

    fn absolute_guest(base: &GuestPath, raw: &[u8]) -> Result<GuestPath, RuntimePathError> {
        let raw = std::str::from_utf8(raw).map_err(|_| RuntimePathError::Invalid)?;
        let combined = if raw.starts_with('/') {
            raw.to_owned()
        } else {
            format!("{}/{}", base.as_str().trim_end_matches('/'), raw)
        };
        GuestPath::new(&combined).map_err(|_| RuntimePathError::Invalid)
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
        OrdinaryContext::layered(upper, lowers).map(Arc::new).map(Self::Ordinary)
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

#[cfg(test)]
mod name_bind_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hl-name-bind-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn aliases_follow_live_siblings_and_mounts_win() {
        let root = fixture("root");
        let directory = root.join("dir");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("probe.so.1"), b"guest").unwrap();
        let host = fixture("host");
        let projected = host.join("projected");
        std::fs::write(&projected, b"projected").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        context
            .add_name_binds(&format!("{}\tprobe.so\tprobe.so.1\tprobe.so.2", projected.display()))
            .unwrap();
        let base = GuestPath::new("/").unwrap();
        let binding = context.name_binding(&base, b"/dir/probe.so").unwrap().unwrap();
        assert_eq!(binding.guest.as_str(), "/dir/probe.so");
        assert_eq!(binding.host, projected);

        std::fs::remove_file(directory.join("probe.so.1")).unwrap();
        assert!(context.name_binding(&base, b"/dir/probe.so").unwrap().is_none());
        std::fs::write(directory.join("probe.so.2"), b"late").unwrap();
        assert!(context.name_binding(&base, b"dir/probe.so").unwrap().is_some());

        let exact = fixture("exact");
        context.mount_directory("/dir", exact.to_str().unwrap(), true).unwrap();
        assert!(context.name_binding(&base, b"/dir/probe.so").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
        std::fs::remove_dir_all(exact).unwrap();
    }

    #[test]
    fn validation_is_bounded_and_transactional() {
        let root = fixture("validation-root");
        let host = fixture("validation-host");
        let projected = host.join("projected");
        std::fs::write(&projected, b"value").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            context.add_name_binds(&format!("{}\tbad/name", projected.display())),
            Err(RuntimePathError::Invalid)
        );
        assert_eq!(
            context.add_name_binds(&format!("{}\tdup\tdup", projected.display())),
            Err(RuntimePathError::Invalid)
        );
        assert_eq!(
            context.add_name_binds(&format!("{}\tdirectory", host.display())),
            Err(RuntimePathError::Invalid)
        );
        assert!(
            context
                .name_binding(&GuestPath::new("/").unwrap(), b"/missing/dup")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
    }

    #[test]
    fn exact_file_bind_preserves_target_and_access() {
        let root = fixture("exact-root");
        std::fs::create_dir(root.join("etc")).unwrap();
        let host = fixture("exact-host");
        let projected = host.join("hosts");
        std::fs::write(&projected, b"127.0.0.1 localhost\n").unwrap();
        let context = OrdinaryContext::new(root.as_os_str().as_bytes()).unwrap();
        context
            .add_name_binds(&format!("rw:{}\t/etc/hosts", projected.display()))
            .unwrap();
        let base = GuestPath::new("/").unwrap();
        let binding = context.name_binding(&base, b"/etc/hosts").unwrap().unwrap();
        assert_eq!(binding.guest.as_str(), "/etc/hosts");
        assert_eq!(binding.host, projected);
        assert!(!binding.read_only);
        assert!(context.name_binding(&base, b"/tmp/hosts").unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host).unwrap();
    }
}
