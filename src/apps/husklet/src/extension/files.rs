//! Files beneath one workspace's storage directory.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_extension::RelativePath;
use hl_extension::port::{Entry, FileRange, HostError, WorkspaceFiles};

/// The workspace file port, rooted at one directory.
///
/// [`RelativePath`] already refuses traversal and absolute paths when it is
/// constructed, and `Authority::permit_path` confines a call to the declared
/// roots. Neither check can see a symbolic link, so every path this adapter is
/// about to touch is resolved on the real filesystem and re-checked against the
/// canonical root. Syntax is not containment.
pub struct WorkspaceDirectory {
    root: PathBuf,
    root_device: u64,
    root_inode: u64,
    mutations: Mutex<()>,
}

const PATH_DEPTH_LIMIT: usize = 128;
const READ_BYTES_LIMIT: usize = (1 << 20) - (8 << 10);
const LIST_ENTRIES_LIMIT: usize = 4096;
const LIST_PATH_BYTES_LIMIT: usize = 512 << 10;

impl WorkspaceDirectory {
    /// Roots the port at `root`, creating it if it does not exist.
    ///
    /// The root is canonicalized once here so that later containment checks
    /// compare two resolved paths; a root reached through a symbolic link would
    /// otherwise make every legitimate path look like an escape.
    ///
    /// # Errors
    /// Returns the failure to create or resolve the root directory.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let metadata = std::fs::metadata(&root)?;
        Ok(Self {
            root,
            root_device: metadata.dev(),
            root_inode: metadata.ino(),
            mutations: Mutex::new(()),
        })
    }

    /// The directory this port is confined to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves the parent while leaving the final entry itself untouched.
    fn entry(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let parts = path.parts();
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root cannot be mutated".to_owned()));
        };
        let parent = join(&self.root, parents.to_vec());
        let parent = parent.canonicalize().map_err(|error| absence(path, &error))?;
        Ok(confine(&self.root, parent)?.join(name))
    }

    fn publication(&self, path: &RelativePath) -> Result<PathBuf, HostError> {
        let target = self.entry(path)?;
        if std::fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(HostError::Conflict(format!("{path} is a symbolic link")));
        }
        Ok(target)
    }

    fn pinned(&self, path: &RelativePath) -> Result<PinnedEntry, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root cannot be mutated".to_owned()));
        };
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parents {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        let metadata = directory.metadata().map_err(|error| absence(path, &error))?;
        Ok(PinnedEntry {
            directory,
            metadata,
            name: c_name(Path::new(name)).map_err(|error| absence(path, &error))?,
        })
    }

    fn root_directory(&self) -> io::Result<File> {
        let descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            &self.root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        let directory = File::from(descriptor);
        let metadata = directory.metadata()?;
        if (metadata.dev(), metadata.ino()) != (self.root_device, self.root_inode) {
            return Err(io::Error::other(
                "workspace root changed after authority was established",
            ));
        }
        Ok(directory)
    }

    fn directory(&self, path: &RelativePath) -> Result<File, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parts {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        Ok(directory)
    }

    fn opened(&self, path: &RelativePath) -> Result<File, HostError> {
        let parts = path.parts();
        if parts.len() > PATH_DEPTH_LIMIT {
            return Err(HostError::Conflict(format!(
                "{path} exceeds the {PATH_DEPTH_LIMIT}-component limit"
            )));
        }
        let Some((name, parents)) = parts.split_last() else {
            return Err(HostError::Conflict("the workspace root is not a file".into()));
        };
        let mut directory = self.root_directory().map_err(|error| absence(path, &error))?;
        for part in parents {
            directory = open_at(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            )
            .map_err(|error| absence(path, &error))?;
        }
        open_at(&directory, name, rustix::fs::OFlags::RDONLY).map_err(|error| absence(path, &error))
    }
}

impl WorkspaceFiles for WorkspaceDirectory {
    /// # Errors
    /// Returns `HostError::Absent` for a missing directory, `HostError::Conflict`
    /// for a path that resolves outside the root, and a failure otherwise.
    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        let directory = self.directory(path)?;
        let reading = rustix::fs::Dir::read_from(&directory)
            .map_err(io::Error::from)
            .map_err(|error| absence(path, &error))?;
        let mut entries = Vec::new();
        let mut path_bytes = 0usize;
        for entry in reading {
            let entry = entry.map_err(io::Error::from).map_err(|error| absence(path, &error))?;
            if matches!(entry.file_name().to_bytes(), b"." | b"..") {
                continue;
            }
            if entries.len() == LIST_ENTRIES_LIMIT {
                return Err(HostError::Failed(format!(
                    "{path}: directory exceeds the {LIST_ENTRIES_LIMIT}-entry limit"
                )));
            }
            let described = described_at(path, &directory, &entry)?;
            path_bytes = path_bytes.saturating_add(described.path.as_str().len());
            if path_bytes > LIST_PATH_BYTES_LIMIT {
                return Err(HostError::Failed(format!(
                    "{path}: directory paths exceed the {LIST_PATH_BYTES_LIMIT}-byte limit"
                )));
            }
            entries.push(described);
        }
        entries.sort_by(|first, second| first.path.cmp(&second.path));
        Ok(entries)
    }

    /// # Errors
    /// Returns `HostError::Absent` for a missing file, `HostError::Conflict` for
    /// a path that resolves outside the root, and a failure otherwise.
    fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
        let file = self.opened(path)?;
        read_bounded(file).map_err(|error| absence(path, &error))
    }

    fn read_range(
        &self,
        path: &RelativePath,
        offset: u64,
        limit: usize,
        observed: Option<&str>,
    ) -> Result<FileRange, HostError> {
        let mut file = self.opened(path)?;
        let before = file.metadata().map_err(|error| absence(path, &error))?;
        if !before.is_file() {
            return Err(HostError::Conflict(format!("{path} is not a regular file")));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| absence(path, &error))?;
        let mut contents = Vec::with_capacity(limit);
        std::io::Read::by_ref(&mut file)
            .take(limit as u64)
            .read_to_end(&mut contents)
            .map_err(|error| absence(path, &error))?;
        let after = file.metadata().map_err(|error| absence(path, &error))?;
        let identity = file_identity(&before);
        if observed.is_some_and(|value| value != identity) {
            return Err(HostError::Conflict(format!(
                "{path} no longer matches the observed identity"
            )));
        }
        if identity != file_identity(&after) {
            return Err(HostError::Conflict(format!("{path} changed while it was read")));
        }
        let total = before.len();
        let eof = offset.saturating_add(contents.len() as u64) >= total;
        Ok(FileRange {
            path: path.clone(),
            identity,
            offset,
            total,
            eof,
            truncated: !eof,
            contents,
        })
    }

    fn stat(&self, path: &RelativePath) -> Result<Entry, HostError> {
        let metadata = self.opened(path)?.metadata().map_err(|error| absence(path, &error))?;
        Ok(Entry {
            path: path.clone(),
            directory: metadata.is_dir(),
            size: metadata.len(),
            identity: Some(file_identity(&metadata)),
        })
    }

    /// # Errors
    /// Returns `HostError::Absent` when the containing directory does not exist,
    /// `HostError::Conflict` for a path that resolves outside the root, and a
    /// failure otherwise.
    fn write(&self, path: &RelativePath, contents: &[u8]) -> Result<(), HostError> {
        let target = self.publication(path)?;
        atomic_write(&target, contents).map_err(|error| absence(path, &error))
    }

    fn create_observed(&self, path: &RelativePath, contents: &[u8]) -> Result<String, HostError> {
        let target = self.publication(path)?;
        atomic_create(&target, contents).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{path} no longer matches the observed identity"))
            } else {
                absence(path, &error)
            }
        })
    }

    fn mkdir(&self, path: &RelativePath) -> Result<(), HostError> {
        let entry = self.pinned(path)?;
        rustix::fs::mkdirat(&entry.directory, &entry.name, rustix::fs::Mode::from_raw_mode(0o777))
            .map_err(io::Error::from)
            .and_then(|()| entry.directory.sync_all())
            .map_err(|error| absence(path, &error))
    }

    fn rename(&self, from: &RelativePath, to: &RelativePath) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let source = self.pinned(from)?;
        let destination = self.pinned(to)?;
        rename_noreplace(&source, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{to} already exists"))
            } else {
                absence(from, &error)
            }
        })?;
        source.directory.sync_all().map_err(|error| absence(from, &error))?;
        if (source.metadata.dev(), source.metadata.ino()) != (destination.metadata.dev(), destination.metadata.ino()) {
            destination.directory.sync_all().map_err(|error| absence(to, &error))?;
        }
        Ok(())
    }

    fn rename_observed(&self, from: &RelativePath, to: &RelativePath, observed: &str) -> Result<String, HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let source = self.pinned(from)?;
        let opened = open_entry_identity(from, &source, observed)?;
        let destination = self.pinned(to)?;
        rename_noreplace(&source, &destination).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                HostError::Conflict(format!("{to} already exists"))
            } else {
                absence(from, &error)
            }
        })?;
        let status = rustix::fs::statat(
            &destination.directory,
            &destination.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        .map_err(|error| absence(to, &error))?;
        if (status.st_dev, status.st_ino) != (opened.dev(), opened.ino()) {
            rename_noreplace(&destination, &source).map_err(|error| absence(from, &error))?;
            return Err(HostError::Conflict(format!(
                "{from} no longer matches the observed identity"
            )));
        }
        source.directory.sync_all().map_err(|error| absence(from, &error))?;
        if (source.metadata.dev(), source.metadata.ino()) != (destination.metadata.dev(), destination.metadata.ino()) {
            destination.directory.sync_all().map_err(|error| absence(to, &error))?;
        }
        let status = rustix::fs::statat(
            &destination.directory,
            &destination.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
        .map_err(|error| absence(to, &error))?;
        Ok(status_identity(&status))
    }

    fn remove(&self, path: &RelativePath) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        remove_entry(&entry)
            .and_then(|()| entry.directory.sync_all())
            .map_err(|error| absence(path, &error))
    }

    fn remove_observed(&self, path: &RelativePath, observed: &str) -> Result<(), HostError> {
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| HostError::Failed("filesystem mutation lock is poisoned".into()))?;
        let entry = self.pinned(path)?;
        let opened = open_entry_identity(path, &entry, observed)?;
        for _ in 0..128 {
            let quarantine = CString::new(format!(
                ".husklet-remove-{}-{}",
                std::process::id(),
                TEMPORARY.fetch_add(1, Ordering::Relaxed)
            ))
            .expect("fixed prefix has no NUL");
            let captured = PinnedEntry {
                directory: entry.directory.try_clone().map_err(|error| absence(path, &error))?,
                metadata: entry.directory.metadata().map_err(|error| absence(path, &error))?,
                name: quarantine,
            };
            match rename_noreplace(&entry, &captured) {
                Ok(()) => {
                    let status = rustix::fs::statat(
                        &captured.directory,
                        &captured.name,
                        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .map_err(io::Error::from)
                    .map_err(|error| absence(path, &error))?;
                    if (status.st_dev, status.st_ino) != (opened.dev(), opened.ino()) {
                        rename_noreplace(&captured, &entry).map_err(|error| absence(path, &error))?;
                        return Err(HostError::Conflict(format!(
                            "{path} no longer matches the observed identity"
                        )));
                    }
                    return remove_entry(&captured)
                        .and_then(|()| entry.directory.sync_all())
                        .map_err(|error| absence(path, &error));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(absence(path, &error)),
            }
        }
        Err(HostError::Failed(
            "could not reserve a unique removal quarantine".into(),
        ))
    }
}

struct PinnedEntry {
    directory: File,
    metadata: std::fs::Metadata,
    name: CString,
}

#[cfg(test)]
fn pin_entry_with(target: &Path, before_open: impl FnOnce() -> io::Result<()>) -> io::Result<PinnedEntry> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("entry has no parent directory"))?;
    let expected = std::fs::metadata(parent)?;
    before_open()?;
    let directory = File::open(parent)?;
    let actual = directory.metadata()?;
    if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
        return Err(io::Error::other("containing directory changed before operation"));
    }
    Ok(PinnedEntry {
        directory,
        metadata: actual,
        name: c_name(target)?,
    })
}

fn rename_noreplace(source: &PinnedEntry, destination: &PinnedEntry) -> io::Result<()> {
    rustix::fs::renameat_with(
        &source.directory,
        &source.name,
        &destination.directory,
        &destination.name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

fn remove_entry(entry: &PinnedEntry) -> io::Result<()> {
    let status = rustix::fs::statat(&entry.directory, &entry.name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    let flags = if rustix::fs::FileType::from_raw_mode(status.st_mode) == rustix::fs::FileType::Directory {
        rustix::fs::AtFlags::REMOVEDIR
    } else {
        rustix::fs::AtFlags::empty()
    };
    rustix::fs::unlinkat(&entry.directory, &entry.name, flags).map_err(Into::into)
}

fn status_identity(status: &rustix::fs::Stat) -> String {
    format!(
        "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
        status.st_dev,
        status.st_ino,
        status.st_size,
        status.st_mtime,
        status.st_mtime_nsec,
        status.st_ctime,
        status.st_ctime_nsec
    )
}

fn open_entry_identity(
    path: &RelativePath,
    entry: &PinnedEntry,
    observed: &str,
) -> Result<std::fs::Metadata, HostError> {
    let descriptor = rustix::fs::openat(
        &entry.directory,
        &entry.name,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)
    .map_err(|error| absence(path, &error))?;
    let metadata = File::from(descriptor)
        .metadata()
        .map_err(|error| absence(path, &error))?;
    if file_identity(&metadata) != observed {
        return Err(HostError::Conflict(format!(
            "{path} no longer matches the observed identity"
        )));
    }
    Ok(metadata)
}

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

struct Temporary {
    directory: File,
    name: CString,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = rustix::fs::unlinkat(&self.directory, &self.name, rustix::fs::AtFlags::empty());
    }
}

fn atomic_write(target: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_with(target, contents, || Ok(()), |_, _, _| Ok(false))
}

fn atomic_create(target: &Path, contents: &[u8]) -> io::Result<String> {
    let mut identity = None;
    atomic_write_with(
        target,
        contents,
        || Ok(()),
        |directory, temporary, file| {
            let target = c_name(target)?;
            rustix::fs::renameat_with(
                directory,
                temporary,
                directory,
                target,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)?;
            identity = Some(file_identity(&file.metadata()?));
            directory.sync_all()?;
            Ok(true)
        },
    )?;
    identity.ok_or_else(|| io::Error::other("atomic creation did not publish"))
}

fn file_identity(metadata: &std::fs::Metadata) -> String {
    format!(
        "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

fn atomic_write_before_publish(
    target: &Path,
    contents: &[u8],
    before_publish: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?
        .to_owned();
    atomic_write_with(
        target,
        contents,
        || Ok(()),
        |_, temporary, _| before_publish(&parent.join(temporary.to_string_lossy().as_ref())).map(|()| false),
    )
}

fn atomic_write_with(
    target: &Path,
    contents: &[u8],
    before_open: impl FnOnce() -> io::Result<()>,
    before_publish: impl FnOnce(&File, &CStr, &File) -> io::Result<bool>,
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;
    let expected = std::fs::metadata(parent)?;
    before_open()?;
    let directory = File::open(parent)?;
    let actual = directory.metadata()?;
    if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
        return Err(io::Error::other("containing directory changed before publication"));
    }
    let target_name = c_name(target)?;
    if symlink_at(&directory, &target_name)? {
        return Err(io::Error::other("publication target is a symbolic link"));
    }
    let mut opened = None;
    for _ in 0..128 {
        let sequence = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".husklet-write-{}-{sequence}.tmp", std::process::id()))
            .expect("generated temporary names contain no NUL");
        match open_exclusive_at(&directory, &name) {
            Ok(file) => {
                opened = Some((
                    Temporary {
                        directory: directory.try_clone()?,
                        name,
                    },
                    file,
                ));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = opened
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "could not reserve atomic write temporary"))?;
    file.write_all(contents)?;
    file.sync_all()?;
    if before_publish(&directory, &temporary.name, &file)? {
        return Ok(());
    }
    rename_at(&directory, &temporary.name, &target_name)?;
    directory.sync_all()?;
    Ok(())
}

fn c_name(path: &Path) -> io::Result<CString> {
    let name = path.file_name().ok_or_else(|| io::Error::other("file has no name"))?;
    CString::new(name.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name contains NUL"))
}

fn open_exclusive_at(directory: &File, name: &CStr) -> io::Result<File> {
    let descriptor = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    Ok(File::from(descriptor))
}

fn symlink_at(directory: &File, name: &CStr) -> io::Result<bool> {
    match rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(status) => Ok(rustix::fs::FileType::from_raw_mode(status.st_mode) == rustix::fs::FileType::Symlink),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn rename_at(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
    // Both names are interpreted relative to the same pinned directory.
    // rename replaces, rather than follows, `to`.
    rustix::fs::renameat(directory, from, directory, to).map_err(Into::into)
}

fn open_at(directory: &File, name: &str, flags: rustix::fs::OFlags) -> io::Result<File> {
    let descriptor = rustix::fs::openat(
        directory,
        name,
        flags | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    Ok(File::from(descriptor))
}

fn read_bounded(file: File) -> io::Result<Vec<u8>> {
    let mut contents = Vec::new();
    file.take((READ_BYTES_LIMIT + 1) as u64).read_to_end(&mut contents)?;
    if contents.len() > READ_BYTES_LIMIT {
        return Err(io::Error::other(format!(
            "file exceeds the {READ_BYTES_LIMIT}-byte read limit"
        )));
    }
    Ok(contents)
}

/// Appends already-validated components to the root.
fn join(root: &Path, parts: Vec<&str>) -> PathBuf {
    let mut full = root.to_path_buf();
    for part in parts {
        full.push(part);
    }
    full
}

/// Accepts a resolved path only while it is still under the resolved root.
///
/// This is the check a symbolic link would otherwise defeat.
fn confine(root: &Path, resolved: PathBuf) -> Result<PathBuf, HostError> {
    if resolved.starts_with(root) {
        return Ok(resolved);
    }
    Err(HostError::Conflict(format!(
        "{} resolves outside the workspace root",
        resolved.display()
    )))
}

/// A missing file is an absence; anything else is a host failure.
fn absence(path: &RelativePath, error: &io::Error) -> HostError {
    if error.kind() == io::ErrorKind::NotFound {
        return HostError::Absent(path.to_string());
    }
    if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        return HostError::Conflict(format!("{path} contains a symbolic link or non-directory component"));
    }
    HostError::Failed(format!("{path}: {error}"))
}

/// Describes one directory entry relative to the listed path.
fn described_at(parent: &RelativePath, directory: &File, entry: &rustix::fs::DirEntry) -> Result<Entry, HostError> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let joined = if parent.parts().is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    };
    let path = RelativePath::new(joined).map_err(|refusal| HostError::Failed(refusal.to_string()))?;
    let metadata = rustix::fs::statat(directory, entry.file_name(), rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)
        .map_err(|error| HostError::Failed(format!("{path}: {error}")))?;
    if metadata.st_ino != entry.ino() {
        return Err(HostError::Conflict(format!(
            "{path} changed while its directory was listed"
        )));
    }
    Ok(Entry {
        path,
        directory: rustix::fs::FileType::from_raw_mode(metadata.st_mode) == rustix::fs::FileType::Directory,
        size: metadata.st_size.try_into().unwrap_or(u64::MAX),
        identity: Some(format!(
            "v1:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_size,
            metadata.st_mtime,
            metadata.st_mtime_nsec,
            metadata.st_ctime,
            metadata.st_ctime_nsec
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::WorkspaceDirectory;
    use hl_extension::RelativePath;
    use hl_extension::port::{HostError, WorkspaceFiles};

    fn path(value: &str) -> RelativePath {
        RelativePath::new(value).expect("path")
    }

    #[test]
    fn an_ordinary_nested_path_is_accepted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("logs")).expect("directory");
        std::fs::write(root.join("logs/app.log"), b"hello").expect("file");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert_eq!(files.read(&path("logs/app.log")).expect("contents"), b"hello");
        let listed = files.list(&path("logs")).expect("listing");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path.as_str(), "logs/app.log");
        assert_eq!(listed[0].size, 5);
        assert!(!listed[0].directory);
        assert_eq!(files.stat(&path("logs/app.log")).expect("metadata"), listed[0]);

        files.write(&path("logs/written.txt"), b"new").expect("write");
        assert_eq!(std::fs::read(root.join("logs/written.txt")).expect("file"), b"new");
        files.mkdir(&path("logs/nested")).expect("mkdir");
        files
            .rename(&path("logs/written.txt"), &path("logs/renamed.txt"))
            .expect("rename");
        files.remove(&path("logs/renamed.txt")).expect("remove file");
        files.remove(&path("logs/nested")).expect("remove empty directory");
        assert!(!root.join("logs/renamed.txt").exists());
    }

    #[test]
    fn a_symbolic_link_out_of_the_root_is_refused() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let secret = temporary.path().join("secret.txt");
        std::fs::write(&secret, b"private").expect("secret");
        let local = root.join("local.txt");
        std::fs::write(&local, b"local").expect("local target");
        std::os::unix::fs::symlink(&secret, root.join("escape.txt")).expect("link");
        std::os::unix::fs::symlink("local.txt", root.join("local-link.txt")).expect("local link");
        std::os::unix::fs::symlink(temporary.path(), root.join("outside")).expect("link");
        let files = WorkspaceDirectory::new(&root).expect("root");

        // The name is syntactically valid and inside a declared root, so nothing
        // upstream of this adapter can refuse it.
        let escape = path("escape.txt");
        assert!(matches!(files.read(&escape), Err(HostError::Conflict(_))));
        assert!(matches!(files.stat(&escape), Err(HostError::Conflict(_))));
        assert!(matches!(files.write(&escape, b"owned"), Err(HostError::Conflict(_))));
        assert!(matches!(
            files.write(&path("local-link.txt"), b"owned"),
            Err(HostError::Conflict(_))
        ));
        assert!(matches!(files.list(&path("outside")), Err(HostError::Conflict(_))));
        assert!(
            matches!(files.read(&path("outside/secret.txt")), Err(HostError::Conflict(_))),
            "a linked directory does not become a root"
        );
        assert_eq!(
            std::fs::read(&secret).expect("secret"),
            b"private",
            "the refused write must not have reached the target"
        );
        files.remove(&path("escape.txt")).expect("remove link itself");
        assert_eq!(std::fs::read(&secret).expect("secret"), b"private");
        assert_eq!(std::fs::read(&local).expect("local target"), b"local");
    }

    #[test]
    fn concurrent_atomic_writes_publish_one_complete_value_and_leave_no_temporaries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = std::sync::Arc::new(WorkspaceDirectory::new(&root).expect("root"));
        let first = vec![b'a'; 64 * 1024];
        let second = vec![b'b'; 64 * 1024];
        let threads = [first.clone(), second.clone()].map(|contents| {
            let files = std::sync::Arc::clone(&files);
            std::thread::spawn(move || files.write(&path("state.bin"), &contents).expect("atomic write"))
        });
        for thread in threads {
            thread.join().expect("writer");
        }
        let published = std::fs::read(root.join("state.bin")).expect("published");
        assert!(
            published == first || published == second,
            "a reader sees one whole publication"
        );
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn observed_ranges_reject_replacements_and_observed_creation_never_overwrites() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let target = path("state.bin");

        let created = files
            .create_observed(&target, b"first-page/second-page")
            .expect("create against observed absence");
        let first = files.read_range(&target, 0, 10, None).expect("first page");
        assert_eq!(first.identity, created);
        assert_eq!(first.contents, b"first-page");
        assert!(!first.eof);

        files.write(&target, b"replacement").expect("concurrent replacement");
        assert!(matches!(
            files.read_range(&target, 10, 10, Some(&first.identity)),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("state.bin")).expect("replacement"),
            b"replacement"
        );
        assert!(matches!(
            files.create_observed(&target, b"second create"),
            Err(HostError::Conflict(_))
        ));
    }

    #[test]
    fn observed_rename_and_remove_capture_then_validate_without_touching_replacements() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let files = WorkspaceDirectory::new(&root).expect("root");
        let source = path("source");
        files.create_observed(&source, b"original").expect("original");
        let observed = files.stat(&source).expect("stat").identity.expect("identity");

        files.write(&source, b"replacement").expect("replacement");
        assert!(matches!(
            files.rename_observed(&source, &path("destination"), &observed),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("source")).expect("restored replacement"),
            b"replacement"
        );
        assert!(
            !root.join("destination").exists(),
            "stale rename rolls its captured entry back"
        );
        assert!(matches!(
            files.remove_observed(&source, &observed),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(
            std::fs::read(root.join("source")).expect("preserved replacement"),
            b"replacement"
        );
        assert!(std::fs::read_dir(&root).expect("listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-remove-")
        }));

        let current = files.stat(&source).expect("current stat").identity.expect("identity");
        let renamed = files
            .rename_observed(&source, &path("destination"), &current)
            .expect("observed rename");
        assert_eq!(
            renamed,
            files
                .stat(&path("destination"))
                .expect("destination stat")
                .identity
                .expect("identity")
        );
        files
            .remove_observed(&path("destination"), &renamed)
            .expect("observed remove");
        assert!(!root.join("destination").exists());
    }

    #[test]
    fn failed_publication_preserves_the_old_entry_and_cleans_its_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(root.join("target")).expect("old directory");
        std::fs::write(root.join("target/held"), b"old").expect("old contents");
        let files = WorkspaceDirectory::new(&root).expect("root");
        assert!(files.write(&path("target"), b"replacement").is_err());
        assert_eq!(std::fs::read(root.join("target/held")).expect("old contents"), b"old");
        assert!(std::fs::read_dir(&root).expect("root listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn failure_before_publication_preserves_prior_file_bytes_and_cleans_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("state");
        std::fs::write(&target, b"old bytes").expect("old contents");

        let result = super::atomic_write_before_publish(&target, b"new bytes", |_| {
            Err(std::io::Error::other("injected before publication"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).expect("old contents"), b"old bytes");
        assert!(
            std::fs::read_dir(temporary.path())
                .expect("directory listing")
                .all(|entry| {
                    !entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".husklet-write-")
                })
        );
    }

    #[test]
    fn replaced_parent_is_rejected_before_any_raced_path_is_written() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let parent = temporary.path().join("parent");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(parent.join("state"), b"old bytes").expect("old contents");
        std::fs::write(outside.join("state"), b"outside bytes").expect("outside contents");
        let target = parent.join("state");

        let result = super::atomic_write_with(
            &target,
            b"new bytes",
            || {
                std::fs::rename(&parent, &displaced)?;
                std::os::unix::fs::symlink(&outside, &parent)?;
                Ok(())
            },
            |_, _, _| Ok(false),
        );

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(displaced.join("state")).expect("old contents"),
            b"old bytes"
        );
        assert_eq!(
            std::fs::read(outside.join("state")).expect("outside contents"),
            b"outside bytes"
        );
        assert!(std::fs::read_dir(&outside).expect("outside listing").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".husklet-write-")
        }));
    }

    #[test]
    fn final_symlink_raced_in_before_rename_is_replaced_not_followed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("state");
        let outside = temporary.path().join("outside");
        std::fs::write(&target, b"old bytes").expect("old contents");
        std::fs::write(&outside, b"outside bytes").expect("outside contents");

        super::atomic_write_before_publish(&target, b"new bytes", |_| {
            std::fs::remove_file(&target)?;
            std::os::unix::fs::symlink(&outside, &target)?;
            Ok(())
        })
        .expect("atomic publication");

        assert_eq!(std::fs::read(&target).expect("published contents"), b"new bytes");
        assert!(
            !std::fs::symlink_metadata(&target)
                .expect("published metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&outside).expect("outside contents"), b"outside bytes");
    }

    #[test]
    fn unrelated_torn_temporary_is_never_published_or_removed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let torn = root.join(".husklet-write-old.tmp");
        std::fs::write(&torn, b"torn").expect("torn temporary");
        let files = WorkspaceDirectory::new(&root).expect("root");
        files.write(&path("state"), b"new").expect("write");
        assert_eq!(std::fs::read(root.join("state")).expect("state"), b"new");
        assert_eq!(std::fs::read(torn).expect("unowned temporary"), b"torn");
    }

    #[test]
    fn mutation_parent_replacement_is_rejected_before_opening_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let parent = temporary.path().join("parent");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&parent).expect("parent");
        std::fs::create_dir_all(&outside).expect("outside");
        let target = parent.join("entry");

        let result = super::pin_entry_with(&target, || {
            std::fs::rename(&parent, &displaced)?;
            std::os::unix::fs::symlink(&outside, &parent)?;
            Ok(())
        });

        assert!(result.is_err());
        assert!(std::fs::read_dir(&outside).expect("outside listing").next().is_none());
    }

    #[test]
    fn final_symlinks_are_never_followed_by_mutations() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(&outside, b"outside").expect("outside");
        std::os::unix::fs::symlink(&outside, root.join("remove-link")).expect("remove link");
        std::os::unix::fs::symlink(&outside, root.join("rename-link")).expect("rename link");
        std::os::unix::fs::symlink(&outside, root.join("mkdir-link")).expect("mkdir link");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(files.mkdir(&path("mkdir-link")).is_err());
        files.remove(&path("remove-link")).expect("remove link itself");
        files
            .rename(&path("rename-link"), &path("renamed-link"))
            .expect("rename link itself");

        assert_eq!(std::fs::read(&outside).expect("outside"), b"outside");
        assert!(!root.join("remove-link").exists());
        assert!(
            std::fs::symlink_metadata(root.join("renamed-link"))
                .expect("renamed link")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn concurrent_renames_never_overwrite_the_winner() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("first"), b"first").expect("first");
        std::fs::write(root.join("second"), b"second").expect("second");
        let files = std::sync::Arc::new(WorkspaceDirectory::new(&root).expect("root"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let threads = ["first", "second"].map(|source| {
            let files = std::sync::Arc::clone(&files);
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                files.rename(&path(source), &path("winner"))
            })
        });
        barrier.wait();
        let results = threads.map(|thread| thread.join().expect("renamer"));

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(HostError::Conflict(_))))
                .count(),
            1
        );
        let winner = std::fs::read(root.join("winner")).expect("winner");
        assert!(winner == b"first" || winner == b"second");
        assert_eq!(
            [root.join("first"), root.join("second")]
                .into_iter()
                .filter(|source| source.exists())
                .count(),
            1
        );
    }

    #[test]
    fn opened_file_authority_survives_a_final_symlink_swap() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        let target = root.join("state");
        let outside = temporary.path().join("outside");
        std::fs::write(&target, b"original").expect("original");
        std::fs::write(&outside, b"secret material").expect("outside");
        let files = WorkspaceDirectory::new(&root).expect("root");

        let opened = files.opened(&path("state")).expect("open authority");
        let metadata = opened.metadata().expect("opened metadata");
        std::fs::remove_file(&target).expect("remove name");
        std::os::unix::fs::symlink(&outside, &target).expect("raced link");

        assert_eq!(super::read_bounded(opened).expect("opened bytes"), b"original");
        assert_eq!(metadata.len(), 8);
        assert_ne!(metadata.len(), std::fs::metadata(&target).expect("later path").len());
    }

    #[test]
    fn opened_directory_authority_survives_an_ancestor_swap() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let listing = root.join("listing");
        let displaced = root.join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&listing).expect("listing");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(listing.join("public"), b"public").expect("public");
        std::fs::write(outside.join("secret"), b"secret").expect("secret");
        let files = WorkspaceDirectory::new(&root).expect("root");

        let directory = files.directory(&path("listing")).expect("directory authority");
        std::fs::rename(&listing, &displaced).expect("displace");
        std::os::unix::fs::symlink(&outside, &listing).expect("raced ancestor");
        let names = rustix::fs::Dir::read_from(&directory)
            .expect("pinned listing")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "public"));
        assert!(!names.iter().any(|name| name == "secret"));
    }

    #[test]
    fn replaced_workspace_root_never_redirects_read_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let displaced = temporary.path().join("displaced");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("state"), b"public").expect("public");
        std::fs::write(outside.join("state"), b"secret").expect("secret");
        let files = WorkspaceDirectory::new(&root).expect("root");
        std::fs::rename(&root, &displaced).expect("displace root");
        std::os::unix::fs::symlink(&outside, &root).expect("replace root");

        assert!(files.read(&path("state")).is_err());
        assert_eq!(std::fs::read(outside.join("state")).expect("outside"), b"secret");
    }

    #[test]
    fn reads_and_component_walks_are_bounded_before_reply_encoding() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("large"), vec![0; super::READ_BYTES_LIMIT + 1]).expect("large file");
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(matches!(files.read(&path("large")), Err(HostError::Failed(_))));
        let deep = std::iter::repeat_n("x", super::PATH_DEPTH_LIMIT + 1)
            .collect::<Vec<_>>()
            .join("/");
        assert!(matches!(files.read(&path(&deep)), Err(HostError::Conflict(_))));
    }

    #[test]
    fn directory_listing_has_a_hard_entry_bound() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        let listing = root.join("listing");
        std::fs::create_dir_all(&listing).expect("listing");
        for index in 0..=super::LIST_ENTRIES_LIMIT {
            std::fs::write(listing.join(index.to_string()), []).expect("entry");
        }
        let files = WorkspaceDirectory::new(&root).expect("root");

        assert!(matches!(files.list(&path("listing")), Err(HostError::Failed(_))));
    }

    #[test]
    fn a_missing_file_is_absent_rather_than_a_failure() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let files = WorkspaceDirectory::new(temporary.path().join("workspace")).expect("root");
        assert!(matches!(files.read(&path("nothing.txt")), Err(HostError::Absent(_))));
        assert!(matches!(files.list(&path("nowhere")), Err(HostError::Absent(_))));
    }

    #[test]
    fn a_root_reached_through_a_link_still_accepts_its_own_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let real = temporary.path().join("real");
        std::fs::create_dir_all(&real).expect("directory");
        std::fs::write(real.join("app.log"), b"hello").expect("file");
        let linked = temporary.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("link");

        let files = WorkspaceDirectory::new(&linked).expect("root");
        assert_eq!(files.read(&path("app.log")).expect("contents"), b"hello");
    }
}
