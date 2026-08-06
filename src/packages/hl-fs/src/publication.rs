use super::{CreateError, Creation};
use std::ffi::{CStr, CString, OsStr};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 256;
const MAX_COMPONENT_BYTES: usize = 255;

struct Directory(OwnedFd);

impl Directory {
    fn open(absolute: bool) -> io::Result<Self> {
        let name = if absolute { c"/" } else { c"." };
        Self::open_at(rustix::fs::CWD, name).map(Self)
    }

    fn open_at(parent: impl std::os::fd::AsFd, name: &CStr) -> io::Result<OwnedFd> {
        rustix::fs::openat(
            parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(Into::into)
    }

    fn child(&self, name: &CStr) -> Result<(Self, bool), CreateError> {
        self.child_with(
            name,
            |directory| {
                StagingFile::create(directory)
                    .map(drop)
                    .map_err(StagingFile::open_error)
            },
            |directory, name| rustix::fs::mkdirat(&directory.0, name, rustix::fs::Mode::from_raw_mode(0o755)),
            |directory, name| Self::open_at(&directory.0, name),
            Self::sync,
            Self::sync,
        )
    }

    fn child_with(
        &self,
        name: &CStr,
        probe: impl FnOnce(&Directory) -> Result<(), CreateError>,
        mkdir: impl FnOnce(&Directory, &CStr) -> Result<(), rustix::io::Errno>,
        open: impl FnOnce(&Directory, &CStr) -> io::Result<OwnedFd>,
        child_sync: impl FnOnce(&Directory) -> io::Result<()>,
        parent_sync: impl FnOnce(&Directory) -> io::Result<()>,
    ) -> Result<(Self, bool), CreateError> {
        match Self::open_at(&self.0, name) {
            Ok(directory) => Ok((Self(directory), false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                probe(self)?;
                let created = match mkdir(self, name) {
                    Ok(()) => true,
                    Err(rustix::io::Errno::EXIST) => false,
                    Err(error) => return Err(CreateError::Io(error.into())),
                };
                let child = Self(
                    open(self, name)
                        .map_err(|error| Publication::with_ancestors(usize::from(created), CreateError::Io(error)))?,
                );
                child_sync(&child)
                    .map_err(|error| Publication::with_ancestors(usize::from(created), CreateError::Io(error)))?;
                parent_sync(self)
                    .map_err(|error| Publication::with_ancestors(usize::from(created), CreateError::Io(error)))?;
                Ok((child, created))
            }
            Err(error) => Err(CreateError::Io(error)),
        }
    }

    fn sync(&self) -> io::Result<()> {
        rustix::fs::fsync(&self.0).map_err(Into::into)
    }
}

struct Publication {
    directory: Directory,
    target: CString,
    created_ancestors: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Publication {
    fn new(target: &Path) -> Result<Self, CreateError> {
        let path = target.as_os_str().as_bytes();
        if path.len() > MAX_PATH_BYTES {
            return Err(CreateError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication path is too long",
            )));
        }
        let mut parts = target.components().peekable();
        let absolute = matches!(parts.peek(), Some(Component::RootDir));
        if absolute {
            parts.next();
        }
        let mut names = Vec::new();
        for part in parts {
            match part {
                Component::Normal(value) => Self::push(&mut names, value).map_err(CreateError::Io)?,
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(CreateError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "publication path contains traversal",
                    )));
                }
            }
        }
        let target = names
            .pop()
            .ok_or_else(|| CreateError::Io(io::Error::new(io::ErrorKind::InvalidInput, "target has no file name")))?;
        let mut directory = Directory::open(absolute).map_err(CreateError::Io)?;
        let mut created_ancestors = 0;
        for name in names {
            let (child, created) = directory
                .child(&name)
                .map_err(|cause| Self::with_ancestors(created_ancestors, cause))?;
            directory = child;
            created_ancestors += usize::from(created);
        }
        Ok(Self {
            directory,
            target,
            created_ancestors,
        })
    }

    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    }

    fn push(names: &mut Vec<CString>, value: &OsStr) -> io::Result<()> {
        if names.len() == MAX_COMPONENTS || value.as_bytes().len() > MAX_COMPONENT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication path exceeds limits",
            ));
        }
        names.push(Self::name(value)?);
        Ok(())
    }

    fn with_ancestors(count: usize, cause: CreateError) -> CreateError {
        if let CreateError::AncestorsCreated { count: later, cause } = cause {
            return CreateError::AncestorsCreated {
                count: count + later,
                cause,
            };
        }
        if count == 0 {
            cause
        } else {
            CreateError::AncestorsCreated {
                count,
                cause: Box::new(cause),
            }
        }
    }

    fn create(self, bytes: &[u8]) -> Result<Creation, CreateError> {
        self.create_with(bytes, |staged, target| staged.publish(target))
    }

    fn create_with(
        self,
        bytes: &[u8],
        publish: impl FnOnce(&StagingFile<'_>, &CStr) -> Result<io::Result<()>, CreateError>,
    ) -> Result<Creation, CreateError> {
        let result = self.create_inner(bytes, publish);
        result.map_err(|cause| Self::with_ancestors(self.created_ancestors, cause))
    }

    fn create_inner(
        &self,
        bytes: &[u8],
        publish: impl FnOnce(&StagingFile<'_>, &CStr) -> Result<io::Result<()>, CreateError>,
    ) -> Result<Creation, CreateError> {
        let mut file = StagingFile::create(&self.directory).map_err(StagingFile::open_error)?;
        file.prepare(bytes).map_err(CreateError::Io)?;
        let publication = publish(&file, &self.target)?;
        let target_matches = file.matches(&self.target).unwrap_or(false);
        if publication.is_ok() {
            return Self::completed(self.directory.sync());
        }
        self.failure(
            publication.expect_err("unconfirmed publication has an error"),
            target_matches,
            self.directory.sync(),
        )
    }

    fn completed(synchronization: io::Result<()>) -> Result<Creation, CreateError> {
        match synchronization {
            Ok(()) => Ok(Creation::Created),
            Err(error) => Err(CreateError::CreatedButNotDurable(error)),
        }
    }

    fn failure(
        &self,
        error: io::Error,
        target_matches: bool,
        synchronization: io::Result<()>,
    ) -> Result<Creation, CreateError> {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return if target_matches {
                Err(CreateError::Ambiguous(error))
            } else {
                Err(CreateError::AlreadyExists)
            };
        }
        if target_matches {
            return Self::completed(synchronization);
        }
        Err(CreateError::Ambiguous(error))
    }
}

struct StagingFile<'a> {
    directory: &'a Directory,
    file: std::fs::File,
    identity: Identity,
}

impl<'a> StagingFile<'a> {
    fn create(directory: &'a Directory) -> io::Result<Self> {
        Self::create_with(directory, std::fs::File::metadata)
    }

    fn open_error(error: io::Error) -> CreateError {
        match error.kind() {
            io::ErrorKind::Unsupported | io::ErrorKind::IsADirectory | io::ErrorKind::NotFound => {
                CreateError::Unsupported
            }
            _ if matches!(
                error.raw_os_error(),
                Some(value)
                    if value == rustix::io::Errno::INVAL.raw_os_error()
                        || value == rustix::io::Errno::NOSYS.raw_os_error()
            ) =>
            {
                CreateError::Unsupported
            }
            _ => CreateError::Io(error),
        }
    }

    fn procfd_error(error: io::Error) -> CreateError {
        let unsupported = [
            rustix::io::Errno::NOENT,
            rustix::io::Errno::NOTDIR,
            rustix::io::Errno::ACCESS,
            rustix::io::Errno::PERM,
        ]
        .into_iter()
        .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()));
        if unsupported {
            CreateError::Unsupported
        } else {
            CreateError::Io(error)
        }
    }

    fn create_with(
        directory: &'a Directory,
        metadata: impl FnOnce(&std::fs::File) -> io::Result<std::fs::Metadata>,
    ) -> io::Result<Self> {
        let descriptor = rustix::fs::openat(
            &directory.0,
            c".",
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::TMPFILE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)?;
        let file: std::fs::File = descriptor.into();
        let metadata = metadata(&file)?;
        Ok(Self {
            directory,
            file,
            identity: Identity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        })
    }

    fn prepare(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.file.sync_all()
    }

    fn publish(&self, target: &CStr) -> Result<io::Result<()>, CreateError> {
        self.publish_with(target, |path| std::fs::metadata(path))
    }

    fn publish_with(
        &self,
        target: &CStr,
        inspect: impl FnOnce(&Path) -> io::Result<std::fs::Metadata>,
    ) -> Result<io::Result<()>, CreateError> {
        let source = CString::new(format!("/proc/self/fd/{}", self.file.as_raw_fd())).unwrap();
        let source_path = Path::new(OsStr::from_bytes(source.as_c_str().to_bytes()));
        let metadata = inspect(source_path).map_err(Self::procfd_error)?;
        if self.identity
            != (Identity {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        {
            return Err(CreateError::Unsupported);
        }
        Ok(rustix::fs::linkat(
            rustix::fs::CWD,
            &source,
            &self.directory.0,
            target,
            rustix::fs::AtFlags::SYMLINK_FOLLOW,
        )
        .map_err(Into::into))
    }

    fn matches(&self, target: &CStr) -> io::Result<bool> {
        let target = rustix::fs::statat(&self.directory.0, target, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io::Error::from)?;
        Ok(self.identity
            == Identity {
                device: target.st_dev as u64,
                inode: target.st_ino as u64,
            })
    }
}

/// Oracle audit: retained `../engine/src/linux_abi/syscall/fs.c` (`typed_host_creation`, syscall
/// cases 34 `mkdirat` and 37 `linkat`) and
/// `../engine/src/linux_abi/container/vfs/resolve.c` (`resolve_at`, `jail_at`),
/// `../engine/src/linux_abi/host_fd.h` (`linkat`, `unlinkat`, `mkdirat`), and
/// `../engine/src/host/macos/host.c` (`hl_macos_file_link`) were studied. The C
/// namespace owns pinned root/volume and per-walk directory descriptors; the walk bounds components,
/// symlink follows, and volume crossings, closes every transient descriptor on success and error, and
/// preserves host operation ordering and errno. Namespace state is read-only during a walk; no table
/// lock is held across host calls. These nonblocking metadata operations have no partial result,
/// cancellation, signal-wakeup, or architecture-specific branch. Linux uses native *at operations;
/// the macOS adapter validates handles and translates `AT_SYMLINK_FOLLOW`; Windows host shims do not
/// provide equivalent publication. Rust maps pinned parents to `Directory`, staged identity
/// and lifetime to `StagingFile`, bounded traversal to `Publication`, and errno to `CreateError`.
/// The retained case 37 materializes O_TMPFILE with capability-gated `AT_EMPTY_PATH` first, then copies
/// into a named exclusive file when that fails. Rust instead requires filesystem O_TMPFILE support and
/// a live `/proc/self/fd` view, returning `Unsupported` before publication when either prerequisite is
/// absent; it deliberately has no copy fallback. Rust additionally owns data/metadata synchronization.
/// Linux `EINVAL` and `ENOSYS`, as well as the standard unsupported error kinds, classify this
/// missing O_TMPFILE capability as `Unsupported` before any namespace publication.
/// Its unnamed inode has no staging pathname to substitute or clean up. Remaining gap: non-Linux hosts
/// have no equivalent no-replace handle publication. Concurrent namespace mutation can redirect a
/// mkdir-`EEXIST` traversal onto another filesystem after earlier ancestors were created; rollback by
/// pathname would be a substitution race, so `AncestorsCreated` reports the exact partial side effect.
pub(super) struct Publisher;

impl Publisher {
    pub(super) fn create(target: &Path, bytes: &[u8]) -> Result<Creation, CreateError> {
        Publication::new(target)?.create(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{Directory, MAX_COMPONENTS, MAX_PATH_BYTES, Publication, Publisher, StagingFile};
    use crate::{CreateError, Creation, File};
    use std::io;

    #[test]
    fn rootless_publish() {
        use std::os::unix::process::CommandExt;

        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let effective = status.lines().find(|line| line.starts_with("Uid:")).unwrap();
        let uid = effective.split_whitespace().nth(2).unwrap().parse::<u32>().unwrap();
        if uid == 0 {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("publication::tests::rootless_child")
                .arg("--nocapture")
                .env("HL_FS_UID501_CHILD", "1")
                .uid(501)
                .gid(501)
                .status()
                .unwrap();
            assert!(result.success());
            return;
        }
        eprintln!("rootless publication exercised as effective uid {uid}");
        rootless_child_body(uid);
    }

    fn rootless_child_body(uid: u32) {
        assert_ne!(uid, 0);
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        assert_eq!(Publisher::create(&target, b"rootless").unwrap(), Creation::Created);
        assert_eq!(std::fs::read(target).unwrap(), b"rootless");
    }

    #[test]
    fn rootless_child() {
        if std::env::var_os("HL_FS_UID501_CHILD").is_none() {
            return;
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let effective = status.lines().find(|line| line.starts_with("Uid:")).unwrap();
        let uid = effective.split_whitespace().nth(2).unwrap().parse::<u32>().unwrap();
        assert_eq!(uid, 501);
        rootless_child_body(uid);
    }

    #[test]
    fn path_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("x".repeat(MAX_PATH_BYTES));
        assert!(File::from(target).create(b"bytes").is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    }

    #[test]
    fn component_bounds() {
        let temporary = tempfile::tempdir().unwrap();
        let deep = (0..=MAX_COMPONENTS).fold(temporary.path().to_owned(), |path, _| path.join("x"));
        assert!(File::from(deep).create(b"bytes").is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);

        let long = temporary.path().join("x".repeat(256));
        assert!(File::from(long).create(b"bytes").is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    }

    #[test]
    fn metadata_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        assert!(StagingFile::create_with(&publication.directory, |_| Err(io::Error::other("fstat"))).is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    }

    #[test]
    fn anonymous_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        let staged = StagingFile::create(&publication.directory).unwrap();

        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
        staged.publish(&publication.target).unwrap().unwrap();
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[test]
    fn collision_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        let staged = StagingFile::create(&publication.directory).unwrap();
        staged.publish(&publication.target).unwrap().unwrap();
        let error = staged.publish(&publication.target).unwrap().unwrap_err();

        assert!(matches!(
            publication.failure(error, staged.matches(&publication.target).unwrap(), Ok(())),
            Err(CreateError::Ambiguous(_))
        ));
    }

    #[test]
    fn failed_reply() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        assert_eq!(
            publication
                .create_with(b"published", |staged, target| {
                    staged
                        .publish(target)?
                        .expect("real link succeeds before reply is lost");
                    Ok(Err(io::Error::other("lost reply")))
                })
                .unwrap(),
            Creation::Created
        );
        assert_eq!(std::fs::read(temporary.path().join("target")).unwrap(), b"published");
    }

    #[test]
    fn durability_precedence() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        assert!(matches!(
            publication.failure(io::Error::other("lost reply"), true, Err(io::Error::other("fsync"))),
            Err(CreateError::CreatedButNotDurable(_))
        ));
    }

    #[test]
    fn directory_durability() {
        assert!(matches!(
            Publication::completed(Err(io::Error::other("fsync"))),
            Err(CreateError::CreatedButNotDurable(_))
        ));
    }

    #[test]
    fn unsupported_filesystem() {
        assert!(matches!(
            StagingFile::open_error(io::Error::from_raw_os_error(95)),
            CreateError::Unsupported
        ));
        assert!(matches!(
            StagingFile::open_error(io::Error::from_raw_os_error(21)),
            CreateError::Unsupported
        ));
        assert!(matches!(
            StagingFile::open_error(io::Error::from_raw_os_error(rustix::io::Errno::INVAL.raw_os_error())),
            CreateError::Unsupported
        ));
        assert!(matches!(
            StagingFile::open_error(io::Error::from_raw_os_error(rustix::io::Errno::NOSYS.raw_os_error())),
            CreateError::Unsupported
        ));
    }

    #[test]
    fn procfs_unavailable() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        let staged = StagingFile::create(&publication.directory).unwrap();
        let result = staged.publish_with(&publication.target, |_| Err(io::Error::from_raw_os_error(2)));
        assert!(matches!(result, Err(CreateError::Unsupported)));
        assert!(!temporary.path().join("target").exists());
        assert!(matches!(
            StagingFile::procfd_error(io::Error::from_raw_os_error(5)),
            CreateError::Io(_)
        ));
    }

    #[test]
    fn ancestor_side_effects() {
        let temporary = tempfile::tempdir().unwrap();
        let publication = Publication::new(&temporary.path().join("target")).unwrap();
        let result = publication.directory.child_with(
            c"missing",
            |_| Err(CreateError::Unsupported),
            |_, _| unreachable!(),
            |directory, name| Directory::open_at(&directory.0, name),
            Directory::sync,
            Directory::sync,
        );
        assert!(matches!(result, Err(CreateError::Unsupported)));
        assert!(!temporary.path().join("missing").exists());
    }

    #[test]
    fn cross_mount_partial() {
        let temporary = tempfile::tempdir().unwrap();
        let root = Publication::new(&temporary.path().join("target")).unwrap().directory;
        let raced_path = temporary.path().join("raced");
        let (raced, created) = root
            .child_with(
                c"raced",
                |_| Ok(()),
                |_, _| {
                    std::fs::create_dir(&raced_path).unwrap();
                    Err(rustix::io::Errno::EXIST)
                },
                |directory, name| Directory::open_at(&directory.0, name),
                Directory::sync,
                Directory::sync,
            )
            .unwrap();
        assert!(!created);
        let publication = Publication {
            directory: raced,
            target: c"target".to_owned(),
            created_ancestors: 1,
        };
        assert!(matches!(
            publication.create_with(b"bytes", |_, _| Err(CreateError::Unsupported)),
            Err(CreateError::AncestorsCreated { count: 1, cause })
                if matches!(*cause, CreateError::Unsupported)
        ));
        assert!(temporary.path().join("raced").is_dir());
    }

    #[test]
    fn recursive_partial() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("one/two/three/target");
        let publication = Publication::new(&target).unwrap();
        assert!(matches!(
            publication.create_with(b"bytes", |_, _| Err(CreateError::Unsupported)),
            Err(CreateError::AncestorsCreated { count: 3, cause })
                if matches!(*cause, CreateError::Unsupported)
        ));
        assert!(temporary.path().join("one/two/three").is_dir());
    }

    #[test]
    fn traversal_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = Publication::new(&temporary.path().join("target")).unwrap().directory;
        let open = directory.child_with(
            c"opened",
            |_| Ok(()),
            |directory, name| rustix::fs::mkdirat(&directory.0, name, rustix::fs::Mode::from_raw_mode(0o755)),
            |_, _| Err(io::Error::other("open")),
            Directory::sync,
            Directory::sync,
        );
        assert!(matches!(open, Err(CreateError::AncestorsCreated { count: 1, .. })));
        assert!(temporary.path().join("opened").is_dir());

        let sync = directory.child_with(
            c"synced",
            |_| Ok(()),
            |directory, name| rustix::fs::mkdirat(&directory.0, name, rustix::fs::Mode::from_raw_mode(0o755)),
            |directory, name| Directory::open_at(&directory.0, name),
            |_| Err(io::Error::other("sync")),
            Directory::sync,
        );
        assert!(matches!(sync, Err(CreateError::AncestorsCreated { count: 1, .. })));
        assert!(temporary.path().join("synced").is_dir());
    }
}
