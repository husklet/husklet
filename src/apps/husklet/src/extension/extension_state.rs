//! Private bounded persistence for one authenticated extension.

use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_extension::port::{ExtensionState, ExtensionStateStore, HostError};

const MAX_STATE_BYTES: usize = 1024 * 1024;
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

pub struct StateBlob {
    directory: OwnedFd,
    name: CString,
}

impl StateBlob {
    pub fn new(root: &Path, name: &hl_extension::ExtensionName) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let root = open_directory(rustix::fs::CWD, root)?;
        let extensions = child_directory(&root, c"extensions")?;
        let directory = child_directory(&extensions, c"state")?;
        rustix::fs::fchmod(&directory, rustix::fs::Mode::from_raw_mode(0o700))?;
        let name = CString::new(format!("{name}.state")).expect("extension names are NUL-free");
        Ok(Self { directory, name })
    }

    fn failure(error: impl std::fmt::Display) -> HostError {
        HostError::Failed(error.to_string())
    }
}

impl ExtensionStateStore for StateBlob {
    fn read(&self) -> Result<ExtensionState, HostError> {
        let descriptor = match rustix::fs::openat(
            &self.directory,
            &self.name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(ExtensionState { contents: Vec::new() }),
            Err(error) => return Err(Self::failure(error)),
        };
        let status = rustix::fs::fstat(&descriptor).map_err(Self::failure)?;
        let size = status.st_size;
        let kind = rustix::fs::FileType::from_raw_mode(status.st_mode);
        if kind != rustix::fs::FileType::RegularFile {
            return Err(HostError::Failed("extension state is not a regular file".into()));
        }
        if size < 0 || size as u64 > MAX_STATE_BYTES as u64 {
            return Err(HostError::Failed("extension state exceeds the 1 MiB quota".into()));
        }
        let mut contents = Vec::with_capacity(size as usize);
        std::fs::File::from(descriptor)
            .take((MAX_STATE_BYTES + 1) as u64)
            .read_to_end(&mut contents)
            .map_err(Self::failure)?;
        if contents.len() > MAX_STATE_BYTES {
            return Err(HostError::Failed("extension state exceeds the 1 MiB quota".into()));
        }
        Ok(ExtensionState { contents })
    }

    fn write(&self, contents: &[u8]) -> Result<(), HostError> {
        if contents.len() > MAX_STATE_BYTES {
            return Err(HostError::Conflict("extension state is limited to 1 MiB".into()));
        }
        match rustix::fs::statat(&self.directory, &self.name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(status) if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile => {
                return Err(HostError::Failed("extension state is not a regular file".into()));
            }
            Ok(_) | Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(Self::failure(error)),
        }
        let temporary = CString::new(format!(
            ".{}.{}-{}.tmp",
            self.name.to_string_lossy(),
            std::process::id(),
            TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ))
        .expect("generated state filenames are NUL-free");
        let descriptor = rustix::fs::openat(
            &self.directory,
            &temporary,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(Self::failure)?;
        let mut file = std::fs::File::from(descriptor);
        let prepared = file.write_all(contents).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = prepared {
            let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty());
            return Err(Self::failure(error));
        }
        let published = rustix::fs::renameat(&self.directory, &temporary, &self.directory, &self.name)
            .and_then(|()| rustix::fs::fsync(&self.directory));
        if published.is_err() {
            let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty());
        }
        published.map_err(Self::failure)
    }

    fn clear(&self) -> Result<(), HostError> {
        match rustix::fs::unlinkat(&self.directory, &self.name, rustix::fs::AtFlags::empty()) {
            Ok(()) => rustix::fs::fsync(&self.directory).map_err(Self::failure),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(Self::failure(error)),
        }
    }
}

fn open_directory(parent: impl AsFd, name: impl rustix::path::Arg) -> io::Result<OwnedFd> {
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

fn child_directory(parent: impl AsFd, name: &CStr) -> io::Result<OwnedFd> {
    match rustix::fs::mkdirat(&parent, name, rustix::fs::Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    open_directory(parent, name)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;

    use hl_extension::port::ExtensionStateStore as _;

    use super::{StateBlob, MAX_STATE_BYTES};

    fn blob(root: &Path, name: &str) -> StateBlob {
        StateBlob::new(root, &hl_extension::ExtensionName::new(name).unwrap()).unwrap()
    }

    #[test]
    fn state_is_private_durable_bounded_and_idempotently_clearable() {
        let root = tempfile::tempdir().unwrap();
        let state = blob(root.path(), "postgres");
        assert_eq!(state.read().unwrap().contents, Vec::<u8>::new());
        state.write(b"{\"schema\":2,\"cursor\":91}").unwrap();
        drop(state);

        let reopened = blob(root.path(), "postgres");
        assert_eq!(reopened.read().unwrap().contents, b"{\"schema\":2,\"cursor\":91}");
        let path = root.path().join("extensions/state/postgres.state");
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        assert!(reopened.write(&vec![0; MAX_STATE_BYTES + 1]).is_err());
        reopened.clear().unwrap();
        reopened.clear().unwrap();
        assert!(reopened.read().unwrap().contents.is_empty());
    }

    #[test]
    fn extension_identity_isolates_state_and_symlink_targets_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let postgres = blob(root.path(), "postgres");
        let indexer = blob(root.path(), "indexer");
        postgres.write(b"credential-reference").unwrap();
        indexer.write(b"checkpoint").unwrap();
        assert_eq!(postgres.read().unwrap().contents, b"credential-reference");
        assert_eq!(indexer.read().unwrap().contents, b"checkpoint");

        postgres.clear().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"outside").unwrap();
        symlink(outside.path(), root.path().join("extensions/state/postgres.state")).unwrap();
        assert!(postgres.read().is_err());
        assert!(postgres.write(b"replacement").is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside");
    }
}
