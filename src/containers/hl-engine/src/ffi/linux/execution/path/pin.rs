use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_runtime::{
    GuestName, GuestPathBytes, MountSourceId, NodeHandle, NodeKind, OpenIntent, ResolveHostError, RuntimePathError,
    VfsHost,
};

use super::overlay_lease::ParentLease;

/// Hidden name holding an emulated `O_TMPFILE` inode alive; dropping it unlinks the name.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(super) struct AnonymousName {
    directory: File,
    name: CString,
    /// Cleared once the hidden name has been renamed onto a guest-visible one.
    armed: bool,
}

#[cfg(target_os = "linux")]
impl AnonymousName {
    fn create(
        parent: RawFd,
        directory_name: &CString,
        flags: i32,
        mode: u32,
    ) -> Result<(File, Option<Self>), RuntimePathError> {
        // SAFETY: `parent` stays open across the call and `directory_name` is NUL-terminated.
        let directory = unsafe {
            libc::openat(
                parent,
                directory_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        };
        if directory < 0 {
            return Err(super::HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one descriptor not owned elsewhere.
        let directory = unsafe { File::from_raw_fd(directory) };
        let flags = flags & !(libc::O_TMPFILE | libc::O_DIRECTORY) | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
        for _ in 0..64 {
            let name = Self::candidate();
            // SAFETY: `directory` stays open across the call and `name` is NUL-terminated.
            let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
            if descriptor >= 0 {
                // SAFETY: successful openat returned one descriptor not owned elsewhere.
                let file = unsafe { File::from_raw_fd(descriptor) };
                return Ok((
                    file,
                    Some(Self {
                        directory,
                        name,
                        armed: true,
                    }),
                ));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(super::HostError::map(error));
            }
        }
        Err(RuntimePathError::Exists)
    }

    fn candidate() -> CString {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let value = NEXT.fetch_add(1, Ordering::Relaxed);
        CString::new(format!(".hl-tmpfile-{}-{value}", std::process::id())).unwrap_or_else(|_| c".hl-tmpfile".into())
    }
}

#[cfg(target_os = "linux")]
impl Drop for AnonymousName {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // SAFETY: both the directory descriptor and the name outlive this unlink.
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
    }
}

/// Shares one emulated anonymous inode between its description and its link capability.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default)]
pub(super) struct AnonymousSlot(Arc<Mutex<Option<AnonymousName>>>);

#[cfg(target_os = "linux")]
impl AnonymousSlot {
    pub(super) fn set(&self, name: Option<AnonymousName>) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = name;
    }

    pub(super) fn present(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Materializes an emulated anonymous inode by renaming its hidden name onto `name`,
    /// which keeps the published link count at one. Returns `false` when the inode is a
    /// real `O_TMPFILE` and the caller should link it instead.
    pub(super) fn materialize(&self, parent: RawFd, name: &CString) -> Option<Result<(), RuntimePathError>> {
        let mut slot = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let anonymous = slot.as_ref()?;
        // SAFETY: both directory descriptors and both names outlive this rename.
        let result = unsafe {
            libc::renameat2(
                anonymous.directory.as_raw_fd(),
                anonymous.name.as_ptr(),
                parent,
                name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            let mut name = slot.take()?;
            name.armed = false;
            return Some(Ok(()));
        }
        Some(Err(super::HostError::map(std::io::Error::last_os_error())))
    }
}

#[derive(Clone)]
pub(super) struct Host {
    roots: Arc<Vec<Arc<File>>>,
    mounts: Arc<super::source::MountPaths>,
    pins: Arc<PinRegistry>,
    epoch: Arc<AtomicU64>,
}

struct PinRegistry {
    next: AtomicU64,
    entries: Mutex<std::collections::BTreeMap<u64, Arc<PinEntry>>>,
    paths: Mutex<PathCache>,
}

struct PathCache {
    epoch: u64,
    entries: std::collections::HashMap<Vec<u8>, Vec<LayerPin>>,
}

struct PinEntry {
    guest: GuestPathBytes,
    /// The first candidate is the visible node. Directory candidates after it
    /// retain lower directories which may contribute children to the union.
    candidates: Vec<LayerPin>,
}

#[derive(Clone)]
struct LayerPin {
    descriptor: Arc<OwnedFd>,
    /// Zero is upper; positive values preserve lower precedence order.
    layer: usize,
}

const LAYERED_HANDLE_BIT: u64 = 1 << 63;

impl Host {
    pub(super) fn new(root: Arc<File>, mounts: Arc<super::source::MountPaths>) -> Self {
        Self::layered(vec![root], mounts)
    }

    /// Builds a resolver host whose roots are ordered upper then lower.
    ///
    /// Resolver handles retain every directory candidate. This is the
    /// load-bearing distinction from a raw descriptor handle: when an upper
    /// directory exists but one of its children exists only in a lower layer,
    /// `inspect_child` can select that lower child without losing the upper
    /// directory's precedence.
    pub(super) fn layered(roots: Vec<Arc<File>>, mounts: Arc<super::source::MountPaths>) -> Self {
        debug_assert!(!roots.is_empty());
        Self {
            roots: Arc::new(roots),
            mounts,
            pins: Arc::new(PinRegistry {
                next: AtomicU64::new(1),
                entries: Mutex::new(std::collections::BTreeMap::new()),
                paths: Mutex::new(PathCache {
                    epoch: 1,
                    entries: std::collections::HashMap::new(),
                }),
            }),
            epoch: Arc::new(AtomicU64::new(1)),
        }
    }

    fn duplicate_descriptor(descriptor: RawFd) -> Result<OwnedFd, ResolveHostError> {
        // SAFETY: fcntl observes a live resolver-owned descriptor and returns
        // an independently owned descriptor without retaining pointers.
        let descriptor = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
        if descriptor < 0 {
            return Err(Self::error());
        }
        // SAFETY: successful F_DUPFD_CLOEXEC created one unowned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }

    fn insert(&self, guest: GuestPathBytes, candidates: Vec<LayerPin>) -> Result<NodeHandle, ResolveHostError> {
        if candidates.is_empty() {
            return Err(ResolveHostError::NotFound);
        }
        let identity = self.pins.next.fetch_add(1, Ordering::Relaxed);
        if identity == 0 {
            return Err(ResolveHostError::ResourceLimit);
        }
        self.pins
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(identity, Arc::new(PinEntry { guest, candidates }));
        Ok(NodeHandle::from_raw(LAYERED_HANDLE_BIT | identity))
    }

    fn with_entry<T>(&self, node: NodeHandle, operation: impl FnOnce(&PinEntry) -> T) -> Result<T, ResolveHostError> {
        let entry = self
            .pins
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(node.raw() & !LAYERED_HANDLE_BIT))
            .cloned()
            .ok_or(ResolveHostError::NotFound)?;
        Ok(operation(&entry))
    }

    fn duplicate_candidates(files: &[Arc<File>]) -> Result<Vec<LayerPin>, ResolveHostError> {
        files
            .iter()
            .enumerate()
            .map(|(layer, file)| {
                Self::duplicate_descriptor(file.as_raw_fd()).map(|descriptor| LayerPin {
                    descriptor: Arc::new(descriptor),
                    layer,
                })
            })
            .collect()
    }

    fn cached_directory(&self, guest: &GuestPathBytes, epoch: u64) -> Option<Vec<LayerPin>> {
        let borrowed = {
            let mut cache = self
                .pins
                .paths
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cache.epoch != epoch {
                cache.entries.clear();
                cache.epoch = epoch;
            }
            cache.entries.get(guest.as_bytes()).cloned()
        };
        borrowed.filter(|_| self.epoch.load(Ordering::Acquire) == epoch)
    }

    fn cache_directory(&self, guest: &GuestPathBytes, candidates: &[LayerPin], epoch: u64) {
        const CAPACITY: usize = 4_096;
        if self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let mut cache = self
            .pins
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.epoch != epoch {
            cache.entries.clear();
            cache.epoch = epoch;
        }
        if self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        if cache.entries.len() >= CAPACITY {
            cache.entries.clear();
        }
        cache.entries.insert(guest.as_bytes().to_vec(), candidates.to_vec());
    }

    fn child_guest(parent: &GuestPathBytes, child: &GuestName) -> Result<GuestPathBytes, ResolveHostError> {
        let mut path = parent.as_bytes().to_vec();
        if path != b"/" {
            path.push(b'/');
        }
        path.extend_from_slice(child.as_bytes());
        GuestPathBytes::new(&path).map_err(|_| ResolveHostError::ResourceLimit)
    }

    #[cfg(target_os = "linux")]
    fn child(directory: RawFd, name: &CString) -> Result<Option<(OwnedFd, NodeKind)>, ResolveHostError> {
        // O_PATH can pin every Linux inode kind without knowing the kind first.
        // Opening before inspecting therefore avoids a redundant fstatat on
        // every component while retaining the same no-follow contract.
        let flags = Self::pin_flags(NodeKind::File);
        // SAFETY: name is terminated and directory remains pinned for openat.
        let child = unsafe { libc::openat(directory, name.as_ptr(), flags) };
        if child < 0 {
            return match Self::error() {
                ResolveHostError::NotFound => Ok(None),
                error => Err(error),
            };
        }
        let observed = match Self::metadata_kind(child) {
            Ok(kind) => kind,
            Err(error) => {
                // SAFETY: successful openat returned one uniquely owned descriptor.
                unsafe { libc::close(child) };
                return Err(error);
            }
        };
        // SAFETY: successful openat returned one uniquely owned descriptor.
        Ok(Some((unsafe { OwnedFd::from_raw_fd(child) }, observed)))
    }

    #[cfg(target_os = "macos")]
    fn child(directory: RawFd, name: &CString) -> Result<Option<(OwnedFd, NodeKind)>, ResolveHostError> {
        let observed = match Self::child_kind(directory, name) {
            Ok(kind) => kind,
            Err(ResolveHostError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let flags = Self::pin_flags(observed);
        // SAFETY: name is terminated and directory remains pinned for openat.
        let child = unsafe { libc::openat(directory, name.as_ptr(), flags) };
        if child < 0 {
            return Err(Self::error());
        }
        let observed = match Self::metadata_kind(child) {
            Ok(kind) => kind,
            Err(error) => {
                // SAFETY: successful openat returned one uniquely owned descriptor.
                unsafe { libc::close(child) };
                return Err(error);
            }
        };
        // SAFETY: successful openat returned one uniquely owned descriptor.
        Ok(Some((unsafe { OwnedFd::from_raw_fd(child) }, observed)))
    }

    fn marker(directory: RawFd, name: &GuestName) -> Result<bool, ResolveHostError> {
        let mut marker = b".wh.".to_vec();
        marker.extend_from_slice(name.as_bytes());
        let marker = CString::new(marker).map_err(|_| ResolveHostError::Io)?;
        match Self::child_kind(directory, &marker) {
            Ok(_) => Ok(true),
            Err(ResolveHostError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn opaque(directory: RawFd) -> Result<bool, ResolveHostError> {
        let marker = c".wh..wh..opq";
        match Self::child_kind(directory, marker) {
            Ok(_) => Ok(true),
            Err(ResolveHostError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn open(
        parent: &ParentLease,
        name: &CString,
        intent: OpenIntent,
        mode: u32,
    ) -> Result<File, RuntimePathError> {
        let flags = Self::open_flags(intent)?;
        let mutation =
            OpenIntent::WRITE | OpenIntent::CREATE | OpenIntent::TRUNCATE | OpenIntent::APPEND | OpenIntent::TEMPORARY;
        let parent = if intent.bits() & mutation != 0 {
            parent.mutation()?
        } else {
            Self::visible_parent(parent, name)?
        };
        // SAFETY: the resolved `parent` lease holds its directory fd open and `name` is a
        // live NUL-terminated CString; openat reads both and retains no pointer.
        let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
        if descriptor < 0 {
            return Err(super::HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one descriptor not owned elsewhere.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    /// Opens an `O_TMPFILE` inode, emulating one with a hidden name when the host filesystem
    /// refuses anonymous creation (virtiofs, NFS and CIFS all do).
    #[cfg(target_os = "linux")]
    pub(super) fn open_temporary(
        parent: &ParentLease,
        name: &CString,
        intent: OpenIntent,
        mode: u32,
    ) -> Result<(File, Option<AnonymousName>), RuntimePathError> {
        let flags = Self::open_flags(intent)?;
        let parent = parent.mutation()?;
        // SAFETY: the `parent` lease holds its directory fd open and `name` is a live
        // NUL-terminated CString; openat reads both and retains no pointer.
        let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
        if descriptor >= 0 {
            // SAFETY: successful openat returned one descriptor not owned elsewhere.
            return Ok((unsafe { File::from_raw_fd(descriptor) }, None));
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP | libc::EINVAL | libc::EISDIR)
        ) {
            return Err(super::HostError::map(error));
        }
        AnonymousName::create(parent.as_raw_fd(), name, flags, mode)
    }

    pub(super) fn path(parent: &ParentLease, name: &CString) -> Result<PathBuf, RuntimePathError> {
        let parent = Self::visible_parent(parent, name)?;
        let directory = Self::descriptor_path(parent.as_raw_fd()).map_err(super::HostError::map)?;
        Ok(directory.join(OsStr::from_bytes(name.as_bytes())))
    }

    pub(super) fn mutation_path(parent: &ParentLease, name: &CString) -> Result<PathBuf, RuntimePathError> {
        let directory = if let Ok(parent) = parent.mutation() {
            Self::descriptor_path(parent.as_raw_fd()).map_err(super::HostError::map)?
        } else {
            let root = parent.upper_root().ok_or(RuntimePathError::NotFound)?;
            let root = Self::descriptor_path(root.as_raw_fd()).map_err(super::HostError::map)?;
            let guest = parent.guest().ok_or(RuntimePathError::Invalid)?;
            root.join(OsStr::from_bytes(
                guest.as_bytes().strip_prefix(b"/").unwrap_or(guest.as_bytes()),
            ))
        };
        Ok(directory.join(OsStr::from_bytes(name.as_bytes())))
    }

    pub(super) fn layer_paths(parent: &ParentLease, name: &CString) -> Result<Vec<PathBuf>, RuntimePathError> {
        let mut paths = Vec::new();
        let selected = Self::descriptor_path(parent.selected().as_raw_fd()).map_err(super::HostError::map)?;
        paths.push(selected.join(OsStr::from_bytes(name.as_bytes())));
        for lower in parent.lower_parents() {
            let directory = Self::descriptor_path(lower.as_raw_fd()).map_err(super::HostError::map)?;
            let path = directory.join(OsStr::from_bytes(name.as_bytes()));
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn visible_parent<'lease>(parent: &'lease ParentLease, name: &CStr) -> Result<&'lease OwnedFd, RuntimePathError> {
        let mut candidates = Vec::with_capacity(parent.lower_parents().len() + 1);
        candidates.push(parent.selected());
        candidates.extend(parent.lower_parents());
        for candidate in candidates {
            if Self::whiteout_at(candidate.as_raw_fd(), name)? {
                return Err(RuntimePathError::NotFound);
            }
            let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: candidate and name remain live and status is writable.
            let result = unsafe {
                libc::fstatat(
                    candidate.as_raw_fd(),
                    name.as_ptr(),
                    status.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result == 0 {
                return Ok(candidate);
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(super::HostError::map(error));
            }
        }
        Err(RuntimePathError::NotFound)
    }

    fn whiteout_at(parent: RawFd, name: &CStr) -> Result<bool, RuntimePathError> {
        let mut marker = b".wh.".to_vec();
        marker.extend_from_slice(name.to_bytes());
        let marker = CString::new(marker).map_err(|_| RuntimePathError::Invalid)?;
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and marker remain live and status is writable.
        let result = unsafe { libc::fstatat(parent, marker.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(super::HostError::map(error))
        }
    }
}

impl VfsHost for Host {
    type ParentLease = ParentLease;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        if self.roots.len() == 1 {
            return Self::duplicate_descriptor(self.roots[0].as_raw_fd()).map(Self::direct_handle);
        }
        let root = GuestPathBytes::new(b"/").map_err(|_| ResolveHostError::Io)?;
        self.insert(root, Self::duplicate_candidates(&self.roots)?)
    }

    fn pin_mount(&self, source: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        let root = self.mounts.root(source).map_err(|error| match error {
            RuntimePathError::NotFound => ResolveHostError::NotFound,
            _ => ResolveHostError::Io,
        })?;
        Self::duplicate_descriptor(root.as_raw_fd()).map(Self::direct_handle)
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        let name = CString::new(component.as_bytes()).map_err(|_| ResolveHostError::Io)?;
        if !Self::is_layered(directory) {
            let descriptor = Self::direct_descriptor(directory);
            let Some((child, kind)) = Self::child(descriptor, &name)? else {
                return Err(ResolveHostError::NotFound);
            };
            return Ok((Self::direct_handle(child), kind));
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        let (guest, candidates, kind) = self.with_entry(directory, |entry| {
            let guest = Self::child_guest(&entry.guest, component)?;
            if let Some(candidates) = self.cached_directory(&guest, epoch) {
                return Ok((guest, candidates, NodeKind::Directory));
            }
            let (candidates, visible) = Self::layer_candidates(&entry.candidates, component, &name)?;
            let kind = visible.ok_or(ResolveHostError::NotFound)?;
            if kind == NodeKind::Directory {
                self.cache_directory(&guest, &candidates, epoch);
            }
            Ok((guest, candidates, kind))
        })??;
        self.insert(guest, candidates).map(|handle| (handle, kind))
    }

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        if !Self::is_layered(link) {
            return Self::read_link(Self::direct_descriptor(link), output);
        }
        self.with_entry(link, |entry| {
            Self::read_link(entry.candidates[0].descriptor.as_raw_fd(), output)
        })?
    }

    fn crosses_mount(&self, directory: NodeHandle, child: NodeHandle) -> Result<bool, ResolveHostError> {
        let directory = self.selected_descriptor(directory)?;
        let child = self.selected_descriptor(child)?;
        Ok(Self::device(directory)? != Self::device(child)?)
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        if !Self::is_layered(parent) {
            return Self::duplicate_descriptor(Self::direct_descriptor(parent)).map(ParentLease::from);
        }
        self.with_entry(parent, |entry| {
            let selected = &entry.candidates[0];
            let selected_descriptor = Self::duplicate_descriptor(selected.descriptor.as_raw_fd())?;
            let upper = entry
                .candidates
                .iter()
                .find(|candidate| candidate.layer == 0)
                .map(|candidate| Self::duplicate_descriptor(candidate.descriptor.as_raw_fd()))
                .transpose()?;
            let lowers = entry
                .candidates
                .iter()
                .filter(|candidate| candidate.layer != 0)
                .map(|candidate| Self::duplicate_descriptor(candidate.descriptor.as_raw_fd()))
                .collect::<Result<Vec<_>, _>>()?;
            let lease = if selected.layer == 0 {
                ParentLease::upper(entry.guest.clone(), selected_descriptor)
            } else {
                ParentLease::lower(entry.guest.clone(), selected.layer - 1, selected_descriptor, upper)
            };
            let upper_root = Self::duplicate_descriptor(self.roots[0].as_raw_fd())?;
            Ok(lease
                .with_lower_parents(lowers)
                .with_upper_root(upper_root)
                .with_epoch(Arc::clone(&self.epoch)))
        })?
    }

    fn close(&self, node: NodeHandle) {
        if !Self::is_layered(node) {
            // SAFETY: direct resolver pins transfer one descriptor and close it once.
            unsafe { libc::close(Self::direct_descriptor(node)) };
            return;
        }
        self.pins
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(node.raw() & !LAYERED_HANDLE_BIT));
    }
}

impl Host {
    /// Collects the layer pins that make up one child in upper-to-lower order.
    fn layer_candidates(
        parents: &[LayerPin],
        component: &GuestName,
        name: &CString,
    ) -> Result<(Vec<LayerPin>, Option<NodeKind>), ResolveHostError> {
        let mut candidates = Vec::new();
        let mut visible = None;
        for parent in parents {
            if visible.is_none() && Self::marker(parent.descriptor.as_raw_fd(), component)? {
                break;
            }
            let Some((child, kind)) = Self::child(parent.descriptor.as_raw_fd(), name)? else {
                continue;
            };
            if visible.is_none() {
                visible = Some(kind);
            }
            let mergeable = visible == Some(kind) && kind == NodeKind::Directory;
            if !mergeable && !candidates.is_empty() {
                continue;
            }
            let opaque = mergeable && Self::opaque(child.as_raw_fd())?;
            candidates.push(LayerPin {
                descriptor: Arc::new(child),
                layer: parent.layer,
            });
            if opaque || !mergeable {
                break;
            }
        }
        Ok((candidates, visible))
    }

    fn is_layered(handle: NodeHandle) -> bool {
        handle.raw() & LAYERED_HANDLE_BIT != 0
    }

    fn direct_handle(descriptor: OwnedFd) -> NodeHandle {
        use std::os::fd::IntoRawFd;
        let descriptor = descriptor.into_raw_fd();
        NodeHandle::from_raw(u64::try_from(descriptor).expect("descriptor is nonnegative") + 1)
    }

    fn direct_descriptor(handle: NodeHandle) -> RawFd {
        debug_assert!(!Self::is_layered(handle));
        i32::try_from(handle.raw() - 1).expect("native descriptor handle")
    }

    fn selected_descriptor(&self, handle: NodeHandle) -> Result<RawFd, ResolveHostError> {
        if Self::is_layered(handle) {
            self.with_entry(handle, |entry| entry.candidates[0].descriptor.as_raw_fd())
        } else {
            Ok(Self::direct_descriptor(handle))
        }
    }

    #[cfg(target_os = "linux")]
    fn pin_flags(_: NodeKind) -> i32 {
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
    }

    #[cfg(target_os = "macos")]
    fn pin_flags(kind: NodeKind) -> i32 {
        libc::O_CLOEXEC
            | if kind == NodeKind::Symlink {
                libc::O_SYMLINK
            } else {
                libc::O_EVTONLY | libc::O_NOFOLLOW
            }
    }

    fn child_kind(directory: RawFd, name: &CStr) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: name is terminated, directory is pinned, and fstatat initializes
        // status on success without retaining either pointer.
        if unsafe { libc::fstatat(directory, name.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstatat initialized every field.
        Ok(Self::kind_from_mode(unsafe { status.assume_init() }.st_mode))
    }

    fn metadata_kind(descriptor: RawFd) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: the successful fstat initialized every field.
        let mode = unsafe { status.assume_init() }.st_mode;
        Ok(Self::kind_from_mode(mode))
    }

    fn device(descriptor: RawFd) -> Result<libc::dev_t, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstat initialized every field.
        Ok(unsafe { status.assume_init() }.st_dev)
    }

    fn kind_from_mode(mode: libc::mode_t) -> NodeKind {
        match mode & libc::S_IFMT {
            libc::S_IFDIR => NodeKind::Directory,
            libc::S_IFREG => NodeKind::File,
            libc::S_IFLNK => NodeKind::Symlink,
            _ => NodeKind::Other,
        }
    }

    #[cfg(target_os = "linux")]
    fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let empty = c"";
        // SAFETY: output is writable for its length and empty requests the pinned
        // O_PATH link itself through Linux AT_EMPTY_PATH semantics.
        let count = unsafe { libc::readlinkat(descriptor, empty.as_ptr(), output.as_mut_ptr().cast(), output.len()) };
        usize::try_from(count).map_err(|_| Self::error())
    }

    #[cfg(target_os = "macos")]
    fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let mut path = vec![0_i8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a terminated path into the bounded output.
        if unsafe { libc::fcntl(descriptor, libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful F_GETPATH produced a terminated C string.
        let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes();
        let target = std::fs::read_link(Path::new(OsStr::from_bytes(bytes))).map_err(Self::map)?;
        let target = target.as_os_str().as_bytes();
        if target.len() > output.len() {
            return Err(ResolveHostError::ResourceLimit);
        }
        output[..target.len()].copy_from_slice(target);
        Ok(target.len())
    }

    fn error() -> ResolveHostError {
        Self::map(std::io::Error::last_os_error())
    }

    fn map(error: std::io::Error) -> ResolveHostError {
        match error.kind() {
            std::io::ErrorKind::NotFound => ResolveHostError::NotFound,
            std::io::ErrorKind::NotADirectory => ResolveHostError::NotDirectory,
            std::io::ErrorKind::PermissionDenied => ResolveHostError::PermissionDenied,
            _ => ResolveHostError::Io,
        }
    }

    fn open_flags(intent: OpenIntent) -> Result<i32, RuntimePathError> {
        let bits = intent.bits();
        let access = match (bits & OpenIntent::READ != 0, bits & OpenIntent::WRITE != 0) {
            (true, true) => libc::O_RDWR,
            (false, true) => libc::O_WRONLY,
            _ => libc::O_RDONLY,
        };
        // Host opens must never pin the engine scheduler if a name is swapped
        // to a FIFO after resolution. Guest blocking is implemented by the
        // named-FIFO domain, so the native descriptor is always acquired
        // nonblocking and revalidated after openat.
        let mut flags = access | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if bits & OpenIntent::CREATE != 0 {
            flags |= libc::O_CREAT;
        }
        if bits & OpenIntent::EXCLUSIVE != 0 {
            flags |= libc::O_EXCL;
        }
        if bits & OpenIntent::TRUNCATE != 0 {
            flags |= libc::O_TRUNC;
        }
        if bits & OpenIntent::APPEND != 0 {
            flags |= libc::O_APPEND;
        }
        if bits & OpenIntent::DIRECTORY != 0 {
            flags |= libc::O_DIRECTORY;
        }
        if bits & OpenIntent::NOFOLLOW != 0 {
            flags |= libc::O_NOFOLLOW;
        }
        Self::platform_open_flags(flags, bits)
    }

    #[cfg(target_os = "linux")]
    fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{descriptor}"))
    }

    #[cfg(target_os = "macos")]
    fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
        let mut path = vec![0_i8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a terminated path into the bounded buffer.
        if unsafe { libc::fcntl(descriptor, libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful F_GETPATH produced a terminated C string.
        let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes();
        Ok(PathBuf::from(OsStr::from_bytes(bytes)))
    }

    #[cfg(target_os = "linux")]
    fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
        if bits & OpenIntent::PATH_ONLY != 0 {
            flags &= libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            flags |= libc::O_PATH;
        }
        if bits & OpenIntent::TEMPORARY != 0 && bits & OpenIntent::PATH_ONLY == 0 {
            flags |= libc::O_TMPFILE;
        }
        Ok(flags)
    }

    #[cfg(target_os = "macos")]
    fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
        if bits & OpenIntent::TEMPORARY != 0 {
            return Err(RuntimePathError::Unsupported);
        }
        if bits & OpenIntent::PATH_ONLY != 0 {
            flags &= !(libc::O_RDONLY | libc::O_WRONLY | libc::O_RDWR);
            if bits & OpenIntent::NOFOLLOW != 0 {
                flags &= !libc::O_NOFOLLOW;
                flags |= libc::O_SYMLINK;
            } else {
                flags |= libc::O_EVTONLY;
            }
        }
        Ok(flags)
    }
}

#[cfg(test)]
mod overlay_tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use hl_runtime::{GuestPathBytes, MountNamespace, ResolveRequest, Resolver, VfsHost};

    use super::Host;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn fixture(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hl-overlay-pin-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn layered_host(upper: &std::path::Path, lower: &std::path::Path) -> Host {
        Host::layered(
            vec![
                std::sync::Arc::new(std::fs::File::open(upper).unwrap()),
                std::sync::Arc::new(std::fs::File::open(lower).unwrap()),
            ],
            std::sync::Arc::new(super::super::source::MountPaths::default()),
        )
    }

    fn resolve(host: &Host, path: &[u8]) -> Result<std::path::PathBuf, hl_runtime::ResolveError> {
        let mounts = MountNamespace::new();
        let resolver = Resolver::new(host.clone(), &mounts);
        let root = GuestPathBytes::new(b"/").unwrap();
        let path = GuestPathBytes::new(path).unwrap();
        let resolved = resolver.resolve(ResolveRequest {
            path: &path,
            base: &root,
            nofollow_final: true,
            no_symlinks: false,
            allow_missing_final: false,
        })?;
        let parent = resolved.duplicate_parent().map_err(hl_runtime::ResolveError::Host)?;
        let name = resolved.final_name().map_or_else(
            || c".".to_owned(),
            |name| std::ffi::CString::new(name.as_bytes()).unwrap(),
        );
        Host::path(&parent, &name).map_err(|_| hl_runtime::ResolveError::Host(hl_runtime::ResolveHostError::Io))
    }

    #[test]
    fn layered_directory_retains_lower_candidates() {
        let upper = fixture("upper");
        let lower = fixture("lower");
        std::fs::create_dir(upper.join("etc")).unwrap();
        std::fs::create_dir(lower.join("etc")).unwrap();
        std::fs::write(lower.join("etc/lower"), b"lower").unwrap();
        std::fs::write(upper.join("etc/upper"), b"upper").unwrap();
        let host = layered_host(&upper, &lower);
        assert_eq!(resolve(&host, b"/etc/lower").unwrap(), lower.join("etc/lower"));
        assert_eq!(resolve(&host, b"/etc/upper").unwrap(), upper.join("etc/upper"));
        std::fs::remove_dir_all(upper).unwrap();
        std::fs::remove_dir_all(lower).unwrap();
    }

    #[test]
    fn upper_whiteout_and_opaque_directory_hide_lower_children() {
        let upper = fixture("whiteout-upper");
        let lower = fixture("whiteout-lower");
        std::fs::create_dir(upper.join("dir")).unwrap();
        std::fs::create_dir(lower.join("dir")).unwrap();
        std::fs::write(upper.join("dir/.wh.hidden"), b"").unwrap();
        std::fs::write(lower.join("dir/hidden"), b"lower").unwrap();
        std::fs::write(lower.join("dir/cut"), b"lower").unwrap();
        let host = layered_host(&upper, &lower);
        assert!(resolve(&host, b"/dir/hidden").is_err());
        std::fs::write(upper.join("dir/.wh..wh..opq"), b"").unwrap();
        let host = layered_host(&upper, &lower);
        assert!(resolve(&host, b"/dir/cut").is_err());
        std::fs::remove_dir_all(upper).unwrap();
        std::fs::remove_dir_all(lower).unwrap();
    }

    #[test]
    fn layered_pin_syscalls_do_not_hold_registry_lock() {
        let upper = fixture("concurrent-upper");
        let lower = fixture("concurrent-lower");
        let host = layered_host(&upper, &lower);
        let root = host.pin_root().unwrap();
        let closing = host.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            entered_rx.recv().unwrap();
            closing.close(root);
            closed_tx.send(()).unwrap();
        });

        host.with_entry(root, |_| {
            entered_tx.send(()).unwrap();
            closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        })
        .unwrap();
        closer.join().unwrap();
        std::fs::remove_dir_all(upper).unwrap();
        std::fs::remove_dir_all(lower).unwrap();
    }

    #[test]
    fn directory_cache_is_bounded_and_epoch_invalidated() {
        let upper = fixture("cache-upper");
        let lower = fixture("cache-lower");
        std::fs::create_dir_all(lower.join("a/b")).unwrap();
        let host = layered_host(&upper, &lower);
        assert_eq!(resolve(&host, b"/a/b").unwrap(), lower.join("a/b"));
        let epoch = host.epoch.load(Ordering::Acquire);
        let guest = GuestPathBytes::new(b"/a").unwrap();
        assert!(host.cached_directory(&guest, epoch).is_some());

        host.epoch.fetch_add(1, Ordering::Release);
        assert!(host.cached_directory(&guest, epoch).is_none());

        let current = host.epoch.load(Ordering::Acquire);
        for index in 0..=4_096 {
            let path = GuestPathBytes::new(format!("/cached/{index}").as_bytes()).unwrap();
            host.cache_directory(&path, &[], current);
        }
        let cache = host
            .pins
            .paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(cache.entries.len() <= 4_096);
        drop(cache);
        std::fs::remove_dir_all(upper).unwrap();
        std::fs::remove_dir_all(lower).unwrap();
    }

    #[test]
    fn new_root_host_has_an_independent_namespace_generation() {
        let upper = fixture("generation-upper");
        let lower = fixture("generation-lower");
        std::fs::create_dir_all(lower.join("a/b")).unwrap();
        let first = layered_host(&upper, &lower);
        assert_eq!(resolve(&first, b"/a/b").unwrap(), lower.join("a/b"));
        let second = layered_host(&upper, &lower);
        assert!(
            second
                .pins
                .paths
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries
                .is_empty()
        );
        std::fs::remove_dir_all(upper).unwrap();
        std::fs::remove_dir_all(lower).unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod anonymous_tests {
    use super::{AnonymousName, AnonymousSlot};
    use hl_runtime::RuntimePathError;
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hl-anon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("scratch")).unwrap();
        root
    }

    fn create(root: &Path) -> (File, AnonymousName) {
        let parent = File::open(root).unwrap();
        let (file, name) = AnonymousName::create(
            parent.as_raw_fd(),
            &CString::new("scratch").unwrap(),
            libc::O_RDWR,
            0o600,
        )
        .unwrap();
        (file, name.unwrap())
    }

    #[test]
    fn hidden_name_is_removed_when_the_inode_is_never_materialized() {
        let root = scratch();
        let (mut file, name) = create(&root);
        file.write_all(b"body").unwrap();
        assert_eq!(std::fs::read_dir(root.join("scratch")).unwrap().count(), 1);
        drop(name);
        assert_eq!(std::fs::read_dir(root.join("scratch")).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn materializing_leaves_exactly_one_link() {
        let root = scratch();
        let (mut file, name) = create(&root);
        file.write_all(b"tmpfile-body").unwrap();
        let slot = AnonymousSlot::default();
        slot.set(Some(name));
        assert!(slot.present());
        let directory = File::open(root.join("scratch")).unwrap();
        slot.materialize(directory.as_raw_fd(), &CString::new("materialized").unwrap())
            .unwrap()
            .unwrap();
        assert!(!slot.present());
        let published = root.join("scratch/materialized");
        assert_eq!(std::fs::read(&published).unwrap(), b"tmpfile-body");
        assert_eq!(std::fs::metadata(&published).unwrap().nlink(), 1);
        drop(slot);
        assert!(published.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn materializing_onto_an_existing_name_reports_exists() {
        let root = scratch();
        let (_file, name) = create(&root);
        std::fs::write(root.join("scratch/taken"), b"other").unwrap();
        let slot = AnonymousSlot::default();
        slot.set(Some(name));
        let directory = File::open(root.join("scratch")).unwrap();
        assert_eq!(
            slot.materialize(directory.as_raw_fd(), &CString::new("taken").unwrap())
                .unwrap(),
            Err(RuntimePathError::Exists)
        );
        assert!(slot.present());
        assert_eq!(std::fs::read(root.join("scratch/taken")).unwrap(), b"other");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_real_anonymous_inode_defers_to_linking() {
        assert!(
            AnonymousSlot::default()
                .materialize(-1, &CString::new("unused").unwrap())
                .is_none()
        );
    }
}
