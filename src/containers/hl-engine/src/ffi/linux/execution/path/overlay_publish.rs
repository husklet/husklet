use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};

use super::overlay_lease::ParentLease;

const COPY_BUFFER_SIZE: usize = 64 * 1024;
#[cfg(test)]
const OPAQUE_NAME: &CStr = c".wh..wh..opq";
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

    pub(super) fn commit(mut self) -> io::Result<()> {
        self.staged.sync_all()?;
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
        sync_directory(self.parent)
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
pub(super) fn prepare_write(lease: &mut ParentLease, name: &CStr) -> io::Result<bool> {
    validate_name(name)?;
    if lease.mutation().is_err() {
        let parent = materialize_guest_parent(lease)?;
        lease.install_upper(parent);
    }
    let upper = lease
        .mutation()
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    if entry_exists(upper, name)? {
        return Ok(false);
    }
    for lower in lease.lower_parents() {
        let source = match open_regular(lower, name)? {
            Some(source) => source,
            None => continue,
        };
        CopyUp::stage(lease, name, &source)?.commit()?;
        return Ok(true);
    }
    Ok(false)
}

fn materialize_guest_parent(lease: &ParentLease) -> io::Result<OwnedFd> {
    let root = lease
        .upper_root()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
    let mut current = root.try_clone()?;
    let guest = lease
        .guest()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    for component in guest.as_bytes().split(|byte| *byte == b'/').filter(|component| !component.is_empty()) {
        let name = CString::new(component).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        materialize_parent(&current, &name, 0o755)?;
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
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
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
pub(super) fn materialize_parent(parent: &impl AsRawFd, name: &CStr, mode: u32) -> io::Result<()> {
    validate_name(name)?;
    // SAFETY: the parent descriptor and terminated name remain live, and
    // mkdirat retains neither after returning.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) } == 0 {
        return sync_directory(parent);
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
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(libc::ENOTDIR))
    }
}

/// Replaces one upper entry with a marker that hides every lower copy.
#[cfg(test)]
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
        .and_then(|()| remove_entry(parent, name))
        .and_then(|()| rename(parent, &staged_name, &marker))
        .and_then(|()| sync_directory(parent));
    if result.is_err() {
        unlink(parent, &staged_name, 0);
    }
    result
}

/// Marks an upper directory opaque so children from lower layers stay hidden.
#[cfg(test)]
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

fn copy_content(source: &File, target: &File) -> io::Result<()> {
    let mut offset = 0_i64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        // SAFETY: buffer is writable, both descriptors remain owned, and pread
        // does not alter the source open-file-description offset.
        let count = unsafe {
            libc::pread(
                source.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                offset,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(());
        }
        let count = usize::try_from(count).expect("positive read count fits usize");
        let mut written = 0;
        while written < count {
            // SAFETY: the unwritten buffer suffix is readable and target lives.
            let result = unsafe {
                libc::write(
                    target.as_raw_fd(),
                    buffer[written..count].as_ptr().cast(),
                    count - written,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if result == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            written += usize::try_from(result).expect("positive write count fits usize");
        }
        offset = offset
            .checked_add(i64::try_from(count).expect("read count fits offset"))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EFBIG))?;
    }
}

fn copy_metadata(source: &File, target: &File) -> io::Result<()> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes status and retains no descriptor.
    if unsafe { libc::fstat(source.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized status.
    let status = unsafe { status.assume_init() };
    // SAFETY: target is owned and fchmod retains nothing.
    if unsafe { libc::fchmod(target.as_raw_fd(), status.st_mode & 0o7777) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    let times = [
        libc::timespec {
            tv_sec: status.st_atime,
            tv_nsec: status.st_atime_nsec,
        },
        libc::timespec {
            tv_sec: status.st_mtime,
            tv_nsec: status.st_mtime_nsec,
        },
    ];
    #[cfg(target_os = "macos")]
    let times = [status.st_atimespec, status.st_mtimespec];
    // SAFETY: times has exactly two initialized entries and target is live.
    if unsafe { libc::futimens(target.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    super::overlay_xattr::copy(source, target)
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

#[cfg(test)]
fn remove_entry(parent: &impl AsRawFd, name: &CStr) -> io::Result<()> {
    // SAFETY: parent and name remain live and unlinkat retains neither.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(());
    }
    // SAFETY: a directory removal uses the same live confined inputs.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
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

fn sync_directory(directory: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fsync operates on the owned descriptor and retains nothing.
    if unsafe { libc::fsync(directory.as_raw_fd()) } == 0 {
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::GuestPathBytes;

    use super::{CopyUp, materialize_parent, publish_opaque, publish_whiteout};
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
        let mut source = OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&lower_path).unwrap();
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

    #[test]
    fn whiteout_and_opaque_markers_publish_only_in_upper() {
        let root = Root::new();
        fs::write(root.0.join("upper/item"), b"upper").unwrap();
        let lease = ParentLease::upper(
            GuestPathBytes::new(b"/").unwrap(),
            File::open(root.0.join("upper")).unwrap().into(),
        );

        publish_whiteout(&lease, c"item").unwrap();
        publish_opaque(lease.mutation().unwrap()).unwrap();

        assert!(!root.0.join("upper/item").exists());
        assert!(root.0.join("upper/.wh.item").exists());
        assert!(root.0.join("upper/.wh..wh..opq").exists());
        assert!(fs::read_dir(root.0.join("lower")).unwrap().next().is_none());
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
}
