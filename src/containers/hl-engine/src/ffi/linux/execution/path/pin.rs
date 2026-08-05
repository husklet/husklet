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

#[derive(Clone)]
pub(super) struct Host {
    roots: Arc<Vec<Arc<File>>>,
    mounts: Arc<super::source::MountPaths>,
    pins: Arc<PinRegistry>,
    epoch: Arc<AtomicU64>,
}

struct PinRegistry {
    next: AtomicU64,
    entries: Mutex<std::collections::BTreeMap<u64, PinEntry>>,
}

struct PinEntry {
    guest: GuestPathBytes,
    /// The first candidate is the visible node. Directory candidates after it
    /// retain lower directories which may contribute children to the union.
    candidates: Vec<LayerPin>,
}

struct LayerPin {
    descriptor: OwnedFd,
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
            .unwrap_or_else(|error| error.into_inner())
            .insert(identity, PinEntry { guest, candidates });
        Ok(NodeHandle::from_raw(LAYERED_HANDLE_BIT | identity))
    }

    fn with_entry<T>(&self, node: NodeHandle, operation: impl FnOnce(&PinEntry) -> T) -> Result<T, ResolveHostError> {
        let entries = self.pins.entries.lock().unwrap_or_else(|error| error.into_inner());
        entries
            .get(&(node.raw() & !LAYERED_HANDLE_BIT))
            .map(operation)
            .ok_or(ResolveHostError::NotFound)
    }

    fn duplicate_candidates(files: &[Arc<File>]) -> Result<Vec<LayerPin>, ResolveHostError> {
        files
            .iter()
            .enumerate()
            .map(|(layer, file)| {
                Self::duplicate_descriptor(file.as_raw_fd()).map(|descriptor| LayerPin { descriptor, layer })
            })
            .collect()
    }

    fn child_guest(parent: &GuestPathBytes, child: &GuestName) -> Result<GuestPathBytes, ResolveHostError> {
        let mut path = parent.as_bytes().to_vec();
        if path != b"/" {
            path.push(b'/');
        }
        path.extend_from_slice(child.as_bytes());
        GuestPathBytes::new(&path).map_err(|_| ResolveHostError::ResourceLimit)
    }

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
        // SAFETY: parent and name remain live for the non-retaining openat;
        // success returns one new descriptor with unique ownership.
        let mutation = OpenIntent::WRITE
            | OpenIntent::CREATE
            | OpenIntent::TRUNCATE
            | OpenIntent::APPEND
            | OpenIntent::TEMPORARY;
        let parent = if intent.bits() & mutation != 0 {
            parent.mutation()?
        } else {
            Self::visible_parent(parent, name)?
        };
        let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
        if descriptor < 0 {
            return Err(super::HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one descriptor not owned elsewhere.
        Ok(unsafe { File::from_raw_fd(descriptor) })
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
            root.join(OsStr::from_bytes(guest.as_bytes().strip_prefix(b"/").unwrap_or(guest.as_bytes())))
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
        let result = unsafe {
            libc::fstatat(
                parent,
                marker.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
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

#[cfg(test)]
mod overlay_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::{GuestPathBytes, MountNamespace, ResolveRequest, Resolver};

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
        let name = resolved
            .final_name()
            .map_or_else(|| c".".to_owned(), |name| std::ffi::CString::new(name.as_bytes()).unwrap());
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
        let (guest, candidates, kind) = self.with_entry(directory, |entry| {
            let mut candidates = Vec::new();
            let mut visible = None;
            for parent in &entry.candidates {
                if visible.is_none() && Self::marker(parent.descriptor.as_raw_fd(), component)? {
                    break;
                }
                if let Some((child, kind)) = Self::child(parent.descriptor.as_raw_fd(), &name)? {
                    let directory = kind == NodeKind::Directory;
                    if visible.is_none() {
                        visible = Some(kind);
                    }
                    if visible == Some(kind) && directory {
                        let opaque = Self::opaque(child.as_raw_fd())?;
                        candidates.push(LayerPin {
                            descriptor: child,
                            layer: parent.layer,
                        });
                        if opaque {
                            break;
                        }
                    } else if candidates.is_empty() {
                        candidates.push(LayerPin {
                            descriptor: child,
                            layer: parent.layer,
                        });
                        break;
                    }
                }
            }
            let guest = Self::child_guest(&entry.guest, component)?;
            visible.map(|kind| (guest, candidates, kind)).ok_or(ResolveHostError::NotFound)
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
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(node.raw() & !LAYERED_HANDLE_BIT));
    }
}

impl Host {
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
