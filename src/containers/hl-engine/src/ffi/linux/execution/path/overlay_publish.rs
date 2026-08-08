use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};

use super::overlay_lease::ParentLease;
use copy::{copy_content, copy_metadata};
use tree::{open_directory, read_children, remove_tree};

#[path = "publication/copy.rs"]
mod copy;
#[path = "publication/tree.rs"]
mod tree;

const OPAQUE_NAME: &CStr = c".wh..wh..opq";
const WHITEOUT_PREFIX: &[u8] = b".wh.";
static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

/// A copy-up staged under a pinned upper parent and published by one rename.
pub(super) struct CopyUp<'lease> {
    lease: &'lease ParentLease,
    parent: &'lease OwnedFd,
    final_name: CString,
    staged_name: CString,
    staged: File,
    published: bool,
}

impl<'lease> CopyUp<'lease> {
    pub(super) fn stage(lease: &'lease ParentLease, name: &CStr, source: &File) -> io::Result<Self> {
        validate_name(name)?;
        let parent = lease
            .mutation()
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
        let staged_name = stage_name("copy")?;
        let staged = create_exclusive(parent, &staged_name, 0o600)?;
        if let Err(error) = copy_content(source, &staged).and_then(|()| copy_metadata(source, &staged)) {
            unlink(parent, &staged_name, 0);
            return Err(error);
        }
        Ok(Self {
            lease,
            parent,
            final_name: name.to_owned(),
            staged_name,
            staged,
            published: false,
        })
    }

    /// Publishes the staged copy and reports the upper identity it landed on.
    pub(super) fn commit(mut self) -> io::Result<(u64, u64)> {
        self.staged.sync_all()?;
        let published = {
            use std::os::unix::fs::MetadataExt;
            let status = self.staged.metadata()?;
            (status.dev(), status.ino())
        };
        rename(self.parent, &self.staged_name, &self.final_name)?;
        if let Err(error) = clear_whiteout(self.parent, &self.final_name) {
            // Keep a failed publication hidden. Restoring the private staged
            // name lets Drop remove it; if the rollback rename itself fails,
            // the still-present whiteout continues to hide the upper entry.
            let _ = rename(self.parent, &self.final_name, &self.staged_name);
            return Err(error);
        }
        self.published = true;
        self.lease.publish();
        sync_directory(self.parent)?;
        Ok(published)
    }
}

impl Drop for CopyUp<'_> {
    fn drop(&mut self) {
        if !self.published {
            unlink(self.parent, &self.staged_name, 0);
        }
    }
}

/// Makes a lower-backed regular file writable in the upper before host open.
/// A create for a name absent from every layer only materializes its parent.
///
/// A copy-up reports the lower and upper host identities it joined, which the
/// caller hands to the advisory-lock table; nowhere later are both still known.
pub(super) fn prepare_write(lease: &mut ParentLease, name: &CStr) -> io::Result<Option<CopiedUp>> {
    validate_name(name)?;
    ensure_upper(lease)?;
    let upper = lease
        .mutation()
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    if entry_exists(upper, name)? {
        return Ok(None);
    }
    for lower in lease.lower_parents() {
        let Some(source) = open_regular(lower, name)? else {
            continue;
        };
        let origin = {
            use std::os::unix::fs::MetadataExt;
            let status = source.metadata()?;
            (status.dev(), status.ino())
        };
        let published = CopyUp::stage(lease, name, &source)?.commit()?;
        return Ok(Some(CopiedUp {
            lower: origin,
            upper: published,
        }));
    }
    Ok(None)
}

/// The two host identities one copy-up gave a single guest file.
#[derive(Clone, Copy, Debug)]
pub(super) struct CopiedUp {
    pub(super) lower: (u64, u64),
    pub(super) upper: (u64, u64),
}

/// Materializes the upper parent of a lease whose walk selected a lower
/// directory, so every later mutation has a writable parent to land in.
pub(super) fn ensure_upper(lease: &mut ParentLease) -> io::Result<()> {
    if lease.mutation().is_ok() {
        return Ok(());
    }
    let parent = materialize_guest_parent(lease)?;
    lease.install_upper(parent);
    Ok(())
}

/// Deletes `name` from the merged view. A lower copy the upper removal would
/// expose is hidden by a whiteout instead; `false` asks the caller for the
/// ordinary upper-only `unlinkat`, which keeps the kernel's own errno.
pub(super) fn remove(lease: &mut ParentLease, name: &CStr, directory: bool) -> io::Result<bool> {
    validate_name(name)?;
    let Some(lower) = lower_status(lease, name)? else {
        // With no lower copy and no upper parent the name exists in no layer.
        lease
            .mutation()
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
        return Ok(false);
    };
    let upper = match lease.mutation() {
        Ok(parent) => status_at(parent, name)?,
        Err(_) => None,
    };
    // The merged type decides, so `rmdir` of a file and `unlink` of a directory
    // fail exactly as they would against a single filesystem.
    let visible = upper.unwrap_or(lower);
    if directory != (visible.st_mode & libc::S_IFMT == libc::S_IFDIR) {
        let errno = if directory { libc::ENOTDIR } else { libc::EISDIR };
        return Err(io::Error::from_raw_os_error(errno));
    }
    if directory && !merged_directory_empty(lease, name)? {
        return Err(io::Error::from_raw_os_error(libc::ENOTEMPTY));
    }
    ensure_upper(lease)?;
    publish_whiteout(lease, name)?;
    Ok(true)
}

/// Gives a name about to be created a writable upper parent, and reports
/// whether a lower still provides it as a directory: such a name must be
/// recreated opaque or the lower's stale children reappear inside it.
pub(super) fn prepare_create(lease: &mut ParentLease, name: &CStr) -> io::Result<bool> {
    validate_name(name)?;
    if lease.lower_parents().is_empty() {
        // No lower can contribute a marker or a stale child, and a lease without
        // lowers always selected the upper, so it already has its parent.
        return Ok(false);
    }
    ensure_upper(lease)?;
    Ok(lower_status(lease, name)?.is_some_and(|status| status.st_mode & libc::S_IFMT == libc::S_IFDIR))
}

/// Drops the whiteout stranded over a name that has just been recreated. It
/// runs only after the host call succeeds, so a refused create never resurrects
/// a lower name the marker was still hiding.
pub(super) fn clear_marker(lease: &ParentLease, name: &CStr) -> io::Result<()> {
    if lease.lower_parents().is_empty() {
        return Ok(());
    }
    let parent = lease
        .mutation()
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    clear_whiteout(parent, name)
}

/// Whether a lower layer still provides `name`, so removing the upper copy
/// would expose it again rather than delete it.
pub(super) fn lower_backed(lease: &ParentLease, name: &CStr) -> io::Result<bool> {
    Ok(lower_status(lease, name)?.is_some())
}

/// The first lower parent still providing `name`. A committed layer chain
/// resolves its markers at unpack, so no `.wh.` probe is needed down there.
fn lower_status(lease: &ParentLease, name: &CStr) -> io::Result<Option<libc::stat>> {
    for lower in lease.lower_parents() {
        if let Some(status) = status_at(lower, name)? {
            return Ok(Some(status));
        }
    }
    Ok(None)
}

/// Whether the merged child directory `name` has no visible entry left, which
/// is what decides `rmdir`'s ENOTEMPTY across layers.
fn merged_directory_empty(lease: &ParentLease, name: &CStr) -> io::Result<bool> {
    let mut layers = Vec::with_capacity(lease.lower_parents().len() + 1);
    layers.extend(lease.mutation().ok());
    layers.extend(lease.lower_parents());
    let mut deleted = BTreeSet::new();
    for layer in layers {
        let Some(directory) = open_directory(layer, name)? else {
            continue;
        };
        let (children, opaque) = read_children(&directory)?;
        // Collect this layer's markers first: within one layer a name and its
        // whiteout can be read in either order.
        deleted.extend(
            children
                .iter()
                .filter_map(|child| child.to_bytes().strip_prefix(WHITEOUT_PREFIX))
                .map(<[u8]>::to_vec),
        );
        let visible = children
            .iter()
            .map(|child| child.to_bytes())
            .any(|child| !child.starts_with(WHITEOUT_PREFIX) && !deleted.contains(child));
        if visible {
            return Ok(false);
        }
        if opaque {
            break;
        }
    }
    Ok(true)
}

fn materialize_guest_parent(lease: &ParentLease) -> io::Result<OwnedFd> {
    let root = lease
        .upper_root()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
    let mut current = root.try_clone()?;
    let guest = lease
        .guest()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    for component in guest
        .as_bytes()
        .split(|byte| *byte == b'/')
        .filter(|component| !component.is_empty())
    {
        let name = CString::new(component).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if materialize_parent_created(&current, &name, 0o755)? {
            // Publish each ancestor immediately: opening the new directory or
            // a later component can fail after mkdirat has become visible.
            lease.publish();
        }
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: current and name remain live and success returns a new descriptor.
        let descriptor = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned one unowned descriptor.
        current = unsafe { OwnedFd::from_raw_fd(descriptor) };
    }
    Ok(current)
}

fn entry_exists(parent: &impl AsRawFd, name: &CStr) -> io::Result<bool> {
    status_at(parent, name).map(|status| status.is_some())
}

/// The nofollow status of one child, reporting an absent name as `None`.
fn status_at(parent: &impl AsRawFd, name: &CStr) -> io::Result<Option<libc::stat>> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent and name remain live and status is writable.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        // SAFETY: the successful fstatat initialized every field.
        return Ok(Some(unsafe { status.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(error)
    }
}

fn open_regular(parent: &impl AsRawFd, name: &CStr) -> io::Result<Option<File>> {
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: parent and name remain live and success returns a new descriptor.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // SAFETY: successful openat returned one unowned descriptor.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if metadata.is_file() {
        Ok(Some(file))
    } else {
        Err(io::Error::from_raw_os_error(libc::EISDIR))
    }
}

/// Materializes one already-validated upper ancestor without following links.
#[cfg(test)]
pub(super) fn materialize_parent(parent: &impl AsRawFd, name: &CStr, mode: u32) -> io::Result<()> {
    materialize_parent_created(parent, name, mode).map(|_| ())
}

fn materialize_parent_created(parent: &impl AsRawFd, name: &CStr, mode: u32) -> io::Result<bool> {
    validate_name(name)?;
    // SAFETY: the parent descriptor and terminated name remain live, and
    // mkdirat retains neither after returning.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) } == 0 {
        sync_directory(parent)?;
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EEXIST) {
        return Err(error);
    }
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: status is writable and the syscall retains no pointers.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized status.
    if unsafe { status.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFDIR {
        Ok(false)
    } else {
        Err(io::Error::from_raw_os_error(libc::ENOTDIR))
    }
}

/// Replaces one upper entry with a marker that hides every lower copy. The
/// marker is staged privately and renamed into place, so no lookup ever sees
/// the name both removed and unmarked.
pub(super) fn publish_whiteout(lease: &ParentLease, name: &CStr) -> io::Result<()> {
    validate_name(name)?;
    let parent = lease
        .mutation()
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    let marker = whiteout_name(name)?;
    let staged_name = stage_name("whiteout")?;
    let staged = create_exclusive(parent, &staged_name, 0o600)?;
    let result = staged
        .sync_all()
        .and_then(|()| remove_tree(parent, name))
        .and_then(|()| rename(parent, &staged_name, &marker))
        .and_then(|()| sync_directory(parent));
    if result.is_err() {
        unlink(parent, &staged_name, 0);
        return result;
    }
    // Retire every cached verdict that predates the marker, or a lookup keeps
    // answering from the walk that still saw the name.
    lease.publish();
    Ok(())
}

/// Marks a freshly recreated upper directory opaque before anything can list
/// it, so the lower copy's stale children never reappear inside it.
pub(super) fn publish_opaque_child(parent: &impl AsRawFd, name: &CStr) -> io::Result<()> {
    let directory = open_directory(parent, name)?.ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
    publish_opaque(&directory)
}

/// Marks an upper directory opaque so children from lower layers stay hidden.
pub(super) fn publish_opaque(directory: &impl AsRawFd) -> io::Result<()> {
    let staged_name = stage_name("opaque")?;
    let staged = create_exclusive(directory, &staged_name, 0o600)?;
    let result = staged
        .sync_all()
        .and_then(|()| rename(directory, &staged_name, OPAQUE_NAME))
        .and_then(|()| sync_directory(directory));
    if result.is_err() {
        unlink(directory, &staged_name, 0);
    }
    result
}

fn create_exclusive(parent: &impl AsRawFd, name: &CStr, mode: u32) -> io::Result<File> {
    let flags = libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    // SAFETY: parent and name remain live; success returns unique descriptor ownership.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful openat returned one unowned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn clear_whiteout(parent: &impl AsRawFd, name: &CStr) -> io::Result<()> {
    let marker = whiteout_name(name)?;
    // SAFETY: parent and marker remain live and unlinkat retains neither.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), marker.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error)
    }
}

fn rename(parent: &impl AsRawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    // SAFETY: both names and the pinned parent live through the non-retaining call.
    if unsafe { libc::renameat(parent.as_raw_fd(), from.as_ptr(), parent.as_raw_fd(), to.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Commits a directory's entries. The pinned parent is usually an `O_PATH`
/// capability, which `fsync` refuses with EBADF, so durability goes through a
/// readable reopen of that same directory rather than the capability itself.
fn sync_directory(directory: &impl AsRawFd) -> io::Result<()> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    // SAFETY: the directory capability stays live and openat retains nothing.
    let descriptor = unsafe { libc::openat(directory.as_raw_fd(), c".".as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned one unowned descriptor.
    let readable = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // SAFETY: fsync operates on the owned descriptor and retains nothing.
    if unsafe { libc::fsync(readable.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unlink(parent: &impl AsRawFd, name: &CStr, flags: i32) {
    // SAFETY: parent and name are live and cleanup ignores an absent entry.
    unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
}

fn whiteout_name(name: &CStr) -> io::Result<CString> {
    let mut bytes = b".wh.".to_vec();
    bytes.extend_from_slice(name.to_bytes());
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn stage_name(kind: &str) -> io::Result<CString> {
    let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    CString::new(format!(".hl_{kind}_{}_{}", std::process::id(), sequence))
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn validate_name(name: &CStr) -> io::Result<()> {
    let bytes = name.to_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::GuestPathBytes;

    use super::{
        CopyUp, clear_marker, materialize_parent, prepare_create, prepare_write, publish_opaque_child,
        publish_whiteout, remove,
    };
    use crate::ffi::linux::execution::path::overlay_lease::ParentLease;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("hl_overlay_publish_{}_{}", std::process::id(), sequence));
            fs::create_dir_all(path.join("lower")).unwrap();
            fs::create_dir_all(path.join("upper")).unwrap();
            Self(path)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn copy_up_publishes_upper_without_changing_lower() {
        let root = Root::new();
        let lower_path = root.0.join("lower/item");
        let upper_path = root.0.join("upper/item");
        let mut source = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lower_path)
            .unwrap();
        source.write_all(b"lower-content").unwrap();
        fs::set_permissions(&lower_path, fs::Permissions::from_mode(0o6751)).unwrap();
        let lease = ParentLease::lower(
            GuestPathBytes::new(b"/").unwrap(),
            0,
            File::open(root.0.join("lower")).unwrap().into(),
            Some(File::open(root.0.join("upper")).unwrap().into()),
        );

        CopyUp::stage(&lease, c"item", &source).unwrap().commit().unwrap();

        assert_eq!(fs::read(&lower_path).unwrap(), b"lower-content");
        assert_eq!(fs::read(&upper_path).unwrap(), b"lower-content");
        assert_eq!(fs::metadata(upper_path).unwrap().permissions().mode() & 0o7777, 0o6751);
    }

    /// A lease whose walk selected the lower root, exactly as `duplicate_parent`
    /// builds one for a directory the upper does not have yet.
    fn lower_root_lease(root: &Root) -> ParentLease {
        ParentLease::lower(
            GuestPathBytes::new(b"/").unwrap(),
            0,
            File::open(root.0.join("lower")).unwrap().into(),
            None,
        )
        .with_lower_parents(vec![File::open(root.0.join("lower")).unwrap().into()])
        .with_upper_root(File::open(root.0.join("upper")).unwrap().into())
    }

    #[test]
    fn removing_a_lower_only_name_publishes_a_whiteout_without_touching_the_lower() {
        let root = Root::new();
        fs::write(root.0.join("lower/item"), b"lower").unwrap();
        fs::write(root.0.join("lower/sibling"), b"lower").unwrap();
        let mut lease = lower_root_lease(&root);

        assert!(remove(&mut lease, c"item", false).unwrap());

        assert!(root.0.join("upper/.wh.item").exists());
        assert!(!root.0.join("upper/item").exists());
        assert_eq!(fs::read(root.0.join("lower/item")).unwrap(), b"lower");
        assert!(!root.0.join("upper/.wh.sibling").exists());
    }

    #[test]
    fn removing_an_upper_only_name_defers_to_the_kernel() {
        let root = Root::new();
        fs::write(root.0.join("upper/item"), b"upper").unwrap();
        let mut lease = ParentLease::upper(
            GuestPathBytes::new(b"/").unwrap(),
            File::open(root.0.join("upper")).unwrap().into(),
        );

        assert!(!remove(&mut lease, c"item", false).unwrap());

        // No marker is published for a name no lower provides, so the committed
        // chain invariant is never charged for an upper-only delete.
        assert!(!root.0.join("upper/.wh.item").exists());
        assert!(root.0.join("upper/item").exists());
    }

    #[test]
    fn merged_type_decides_removal_so_rmdir_cannot_delete_a_lower_file() {
        let root = Root::new();
        fs::write(root.0.join("lower/file"), b"lower").unwrap();
        fs::create_dir(root.0.join("lower/directory")).unwrap();
        let mut lease = lower_root_lease(&root);

        assert_eq!(
            remove(&mut lease, c"file", true).unwrap_err().raw_os_error(),
            Some(libc::ENOTDIR),
        );
        assert_eq!(
            remove(&mut lease, c"directory", false).unwrap_err().raw_os_error(),
            Some(libc::EISDIR),
        );
        assert!(root.0.join("lower/file").exists());
    }

    #[test]
    fn rmdir_refuses_a_directory_whose_children_only_the_lower_still_provides() {
        let root = Root::new();
        fs::create_dir_all(root.0.join("lower/tree")).unwrap();
        fs::write(root.0.join("lower/tree/child"), b"lower").unwrap();
        let mut lease = lower_root_lease(&root);

        assert_eq!(
            remove(&mut lease, c"tree", true).unwrap_err().raw_os_error(),
            Some(libc::ENOTEMPTY),
        );

        // Once the child itself is whited out the merged directory is empty, so
        // the same rmdir succeeds and publishes the parent's own marker.
        fs::create_dir_all(root.0.join("upper/tree")).unwrap();
        fs::write(root.0.join("upper/tree/.wh.child"), b"").unwrap();
        let mut lease = ParentLease::upper(
            GuestPathBytes::new(b"/").unwrap(),
            File::open(root.0.join("upper")).unwrap().into(),
        )
        .with_lower_parents(vec![File::open(root.0.join("lower")).unwrap().into()]);

        assert!(remove(&mut lease, c"tree", true).unwrap());
        assert!(root.0.join("upper/.wh.tree").exists());
        assert!(!root.0.join("upper/tree").exists());
    }

    #[test]
    fn recreating_a_lower_directory_clears_its_whiteout_and_asks_for_opacity() {
        let root = Root::new();
        fs::create_dir_all(root.0.join("lower/tree")).unwrap();
        fs::write(root.0.join("lower/tree/stale"), b"lower").unwrap();
        fs::write(root.0.join("lower/plain"), b"lower").unwrap();
        fs::create_dir_all(root.0.join("upper")).unwrap();
        fs::write(root.0.join("upper/.wh.tree"), b"").unwrap();
        let mut lease = lower_root_lease(&root);

        assert!(prepare_create(&mut lease, c"tree").unwrap());
        // A lower-provided file needs no opaque marker, only the cleared name.
        assert!(!prepare_create(&mut lease, c"plain").unwrap());
        // The marker only retires once the create has actually succeeded, so a
        // refused create cannot resurrect the name it was still hiding.
        assert!(root.0.join("upper/.wh.tree").exists());

        fs::create_dir(root.0.join("upper/tree")).unwrap();
        publish_opaque_child(lease.mutation().unwrap(), c"tree").unwrap();
        clear_marker(&lease, c"tree").unwrap();
        assert!(!root.0.join("upper/.wh.tree").exists());
        assert!(root.0.join("upper/tree/.wh..wh..opq").exists());
        assert!(root.0.join("lower/tree/stale").exists());
    }

    #[test]
    fn whiteout_replaces_an_upper_directory_that_still_holds_child_markers() {
        let root = Root::new();
        fs::create_dir_all(root.0.join("lower/tree")).unwrap();
        fs::create_dir_all(root.0.join("upper/tree/nested")).unwrap();
        // A plain rmdir would fail ENOTEMPTY on these and leave `tree` resolving.
        fs::write(root.0.join("upper/tree/.wh.gone"), b"").unwrap();
        fs::write(root.0.join("upper/tree/nested/.wh.deep"), b"").unwrap();
        let lease = ParentLease::upper(
            GuestPathBytes::new(b"/").unwrap(),
            File::open(root.0.join("upper")).unwrap().into(),
        )
        .with_lower_parents(vec![File::open(root.0.join("lower")).unwrap().into()]);

        publish_whiteout(&lease, c"tree").unwrap();

        assert!(!root.0.join("upper/tree").exists());
        assert!(root.0.join("upper/.wh.tree").exists());
        assert!(root.0.join("lower/tree").is_dir());
    }

    #[test]
    fn parent_materialization_rejects_non_directory_collision() {
        let root = Root::new();
        let upper = File::open(root.0.join("upper")).unwrap();
        materialize_parent(&upper, c"cache", 0o750).unwrap();
        assert!(root.0.join("upper/cache").is_dir());
        fs::write(root.0.join("upper/file"), b"x").unwrap();
        assert_eq!(
            materialize_parent(&upper, &CString::new("file").unwrap(), 0o750)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOTDIR),
        );
    }

    #[test]
    fn parent_materialization_publishes_each_visible_ancestor() {
        let root = Root::new();
        fs::create_dir_all(root.0.join("lower/a/b")).unwrap();
        let epoch = Arc::new(AtomicU64::new(4));
        let mut lease = ParentLease::lower(
            GuestPathBytes::new(b"/a/b").unwrap(),
            0,
            File::open(root.0.join("lower/a/b")).unwrap().into(),
            None,
        )
        .with_upper_root(File::open(root.0.join("upper")).unwrap().into())
        .with_epoch(Arc::clone(&epoch));

        assert!(prepare_write(&mut lease, c"new").unwrap().is_none());

        assert!(root.0.join("upper/a/b").is_dir());
        assert_eq!(epoch.load(Ordering::Acquire), 6);
    }
}
