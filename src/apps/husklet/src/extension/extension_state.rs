//! Private bounded persistence for one authenticated extension.

use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_extension::port::{ExtensionCredential, ExtensionPreferences, ExtensionState, ExtensionStateStore, HostError, PreferenceValue};
use sha2::Digest as _;

const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_PREFERENCE_FILE_BYTES: usize = 72 * 1024;
const MAX_PREFERENCES: usize = 64;
const MAX_CREDENTIALS: usize = 64;
const MAX_CREDENTIAL_VALUE_BYTES: usize = 64 * 1024;
const MAX_CREDENTIAL_FILE_BYTES: usize = 4 * 1024 * 1024 + 16 * 1024;
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

pub struct StateBlob {
    directory: OwnedFd,
    name: CString,
    lock_name: CString,
    preferences_name: CString,
    credentials_name: CString,
}

impl StateBlob {
    pub fn new(root: &Path, name: &hl_extension::ExtensionName) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let root = open_directory(rustix::fs::CWD, root)?;
        let extensions = child_directory(&root, c"extensions")?;
        let directory = child_directory(&extensions, c"state")?;
        rustix::fs::fchmod(&directory, rustix::fs::Mode::from_raw_mode(0o700))?;
        let preferences_name = CString::new(format!("{name}.preferences")).expect("extension names are NUL-free");
        let credentials_name = CString::new(format!("{name}.credentials")).expect("extension names are NUL-free");
        let lock_name = CString::new(format!(".{name}.state.lock")).expect("extension names are NUL-free");
        let name = CString::new(format!("{name}.state")).expect("extension names are NUL-free");
        Ok(Self {
            directory,
            name,
            lock_name,
            preferences_name,
            credentials_name,
        })
    }

    fn failure(error: impl std::fmt::Display) -> HostError {
        HostError::Failed(error.to_string())
    }

    pub fn purge(&self) -> Result<(), HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        self.remove_unlocked()?;
        match rustix::fs::unlinkat(&self.directory, &self.preferences_name, rustix::fs::AtFlags::empty()) {
            Ok(()) => rustix::fs::fsync(&self.directory).map_err(Self::failure),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(Self::failure(error)),
        }?;
        match rustix::fs::unlinkat(&self.directory, &self.credentials_name, rustix::fs::AtFlags::empty()) {
            Ok(()) => rustix::fs::fsync(&self.directory).map_err(Self::failure),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(Self::failure(error)),
        }
    }

    fn remove_unlocked(&self) -> Result<(), HostError> {
        match rustix::fs::unlinkat(&self.directory, &self.name, rustix::fs::AtFlags::empty()) {
            Ok(()) => rustix::fs::fsync(&self.directory).map_err(Self::failure),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(Self::failure(error)),
        }
    }

    fn lock(&self) -> Result<std::fs::File, HostError> {
        let descriptor = rustix::fs::openat(
            &self.directory,
            &self.lock_name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(Self::failure)?;
        let status = rustix::fs::fstat(&descriptor).map_err(Self::failure)?;
        if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(HostError::Failed("extension state lock is not a regular file".into()));
        }
        rustix::fs::fchmod(&descriptor, rustix::fs::Mode::from_raw_mode(0o600)).map_err(Self::failure)?;
        Ok(descriptor.into())
    }

    fn read_unlocked(&self) -> Result<ExtensionState, HostError> {
        let descriptor = match rustix::fs::openat(
            &self.directory,
            &self.name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => {
                return Ok(ExtensionState {
                    identity: "absent".into(),
                    contents: Vec::new(),
                });
            }
            Err(error) => return Err(Self::failure(error)),
        };
        let status = rustix::fs::fstat(&descriptor).map_err(Self::failure)?;
        let size = status.st_size;
        if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile {
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
        let identity = identity(&contents);
        Ok(ExtensionState { identity, contents })
    }

    fn read_preferences_unlocked(&self) -> Result<ExtensionPreferences, HostError> {
        let descriptor = match rustix::fs::openat(
            &self.directory,
            &self.preferences_name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => {
                return Ok(ExtensionPreferences {
                    revision: 0,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(Self::failure(error)),
        };
        let status = rustix::fs::fstat(&descriptor).map_err(Self::failure)?;
        if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile
            || status.st_size < 0
            || status.st_size as usize > MAX_PREFERENCE_FILE_BYTES
        {
            return Err(HostError::Failed(
                "extension preferences are invalid or oversized".into(),
            ));
        }
        let mut bytes = Vec::with_capacity(status.st_size as usize);
        std::fs::File::from(descriptor)
            .take((MAX_PREFERENCE_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(Self::failure)?;
        let preferences: ExtensionPreferences = serde_json::from_slice(&bytes).map_err(Self::failure)?;
        if preferences.entries.len() > MAX_PREFERENCES {
            return Err(HostError::Failed(
                "extension preferences exceed the 64-entry quota".into(),
            ));
        }
        Ok(preferences)
    }

    fn publish_preferences_unlocked(&self, preferences: &ExtensionPreferences) -> Result<(), HostError> {
        let bytes = serde_json::to_vec(preferences).map_err(Self::failure)?;
        if bytes.len() > MAX_PREFERENCE_FILE_BYTES {
            return Err(HostError::Conflict(
                "extension preferences exceed their bounded storage quota".into(),
            ));
        }
        let temporary = CString::new(format!(
            ".{}.{}-{}.tmp",
            self.preferences_name.to_string_lossy(),
            std::process::id(),
            TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ))
        .expect("generated preference filenames are NUL-free");
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
        let prepared = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = prepared {
            let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty());
            return Err(Self::failure(error));
        }
        let published = rustix::fs::renameat(&self.directory, &temporary, &self.directory, &self.preferences_name)
            .and_then(|()| rustix::fs::fsync(&self.directory));
        if published.is_err() {
            let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty());
        }
        published.map_err(Self::failure)
    }

    fn read_credentials_unlocked(&self) -> Result<CredentialFile, HostError> {
        let descriptor = match rustix::fs::openat(
            &self.directory, &self.credentials_name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(CredentialFile::default()),
            Err(error) => return Err(Self::failure(error)),
        };
        let status = rustix::fs::fstat(&descriptor).map_err(Self::failure)?;
        if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile
            || status.st_size < 0 || status.st_size as usize > MAX_CREDENTIAL_FILE_BYTES
            || status.st_mode & 0o077 != 0
        {
            return Err(HostError::Failed("extension credential store is not a private bounded regular file".into()));
        }
        let mut bytes = Vec::with_capacity(status.st_size as usize);
        std::fs::File::from(descriptor).take((MAX_CREDENTIAL_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes).map_err(Self::failure)?;
        let file: CredentialFile = serde_json::from_slice(&bytes).map_err(Self::failure)?;
        let mut keys = std::collections::BTreeSet::new();
        if file.entries.len() > MAX_CREDENTIALS
            || file.entries.iter().any(|(key, value)| key.is_empty() || key.len() > 64
                || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                || value.len() > MAX_CREDENTIAL_VALUE_BYTES || !keys.insert(key))
        {
            return Err(HostError::Failed("extension credential store is invalid or oversized".into()));
        }
        Ok(file)
    }

    fn publish_credentials_unlocked(&self, credentials: &CredentialFile) -> Result<(), HostError> {
        let bytes = serde_json::to_vec(credentials).map_err(Self::failure)?;
        if bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
            return Err(HostError::Conflict("extension credentials exceed their 4 MiB storage quota".into()));
        }
        let temporary = CString::new(format!(".{}.{}-{}.tmp", self.credentials_name.to_string_lossy(), std::process::id(), TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)))
            .expect("generated credential filenames are NUL-free");
        let descriptor = rustix::fs::openat(&self.directory, &temporary,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600)).map_err(Self::failure)?;
        let mut file = std::fs::File::from(descriptor);
        let prepared = file.write_all(&bytes).and_then(|()| file.sync_all()); drop(file);
        if let Err(error) = prepared { let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty()); return Err(Self::failure(error)); }
        let published = rustix::fs::renameat(&self.directory, &temporary, &self.directory, &self.credentials_name)
            .and_then(|()| rustix::fs::fsync(&self.directory));
        if published.is_err() { let _ = rustix::fs::unlinkat(&self.directory, &temporary, rustix::fs::AtFlags::empty()); }
        published.map_err(Self::failure)
    }
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct CredentialFile { revision: u64, entries: Vec<(String, Vec<u8>)> }

impl ExtensionStateStore for StateBlob {
    fn read(&self) -> Result<ExtensionState, HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_shared(&lock).map_err(Self::failure)?;
        self.read_unlocked()
    }

    fn write(&self, observed: &str, contents: &[u8]) -> Result<String, HostError> {
        if contents.len() > MAX_STATE_BYTES {
            return Err(HostError::Conflict("extension state is limited to 1 MiB".into()));
        }
        let lock = self.lock()?;
        fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        if self.read_unlocked()?.identity != observed {
            return Err(HostError::Conflict("extension state changed after it was read".into()));
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
        published.map_err(Self::failure)?;
        Ok(identity(contents))
    }

    fn clear(&self, observed: &str) -> Result<(), HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        if self.read_unlocked()?.identity != observed {
            return Err(HostError::Conflict("extension state changed after it was read".into()));
        }
        self.remove_unlocked()
    }

    fn preferences(&self) -> Result<ExtensionPreferences, HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_shared(&lock).map_err(Self::failure)?;
        self.read_preferences_unlocked()
    }

    fn preference_set(&self, observed: u64, key: &str, value: &PreferenceValue) -> Result<u64, HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        let mut preferences = self.read_preferences_unlocked()?;
        if preferences.revision != observed {
            return Err(HostError::Conflict(
                "extension preferences changed after they were read".into(),
            ));
        }
        if let Some(position) = preferences.entries.iter().position(|(current, _)| current == key) {
            preferences.entries[position].1.clone_from(value);
        } else if preferences.entries.len() < MAX_PREFERENCES {
            preferences.entries.push((key.to_owned(), value.clone()));
        } else {
            return Err(HostError::Conflict(
                "extension preferences are limited to 64 entries".into(),
            ));
        }
        preferences.entries.sort_by(|left, right| left.0.cmp(&right.0));
        preferences.revision = preferences
            .revision
            .checked_add(1)
            .ok_or_else(|| HostError::Conflict("extension preference revision is exhausted".into()))?;
        self.publish_preferences_unlocked(&preferences)?;
        Ok(preferences.revision)
    }

    fn preference_remove(&self, observed: u64, key: &str) -> Result<u64, HostError> {
        let lock = self.lock()?;
        fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        let mut preferences = self.read_preferences_unlocked()?;
        if preferences.revision != observed {
            return Err(HostError::Conflict(
                "extension preferences changed after they were read".into(),
            ));
        }
        preferences.entries.retain(|(current, _)| current != key);
        preferences.revision = preferences
            .revision
            .checked_add(1)
            .ok_or_else(|| HostError::Conflict("extension preference revision is exhausted".into()))?;
        self.publish_preferences_unlocked(&preferences)?;
        Ok(preferences.revision)
    }

    fn credential(&self, key: &str) -> Result<ExtensionCredential, HostError> {
        let lock = self.lock()?; fs2::FileExt::lock_shared(&lock).map_err(Self::failure)?;
        let credentials = self.read_credentials_unlocked()?;
        Ok(ExtensionCredential { revision: credentials.revision, value: credentials.entries.iter().find(|(current, _)| current == key).map(|(_, value)| value.clone()) })
    }

    fn credential_set(&self, observed: u64, key: &str, value: &[u8]) -> Result<u64, HostError> {
        let lock = self.lock()?; fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        let mut credentials = self.read_credentials_unlocked()?;
        if credentials.revision != observed { return Err(HostError::Conflict("extension credentials changed after they were read".into())); }
        if let Some((_, current)) = credentials.entries.iter_mut().find(|(current, _)| current == key) { current.clear(); current.extend_from_slice(value); }
        else if credentials.entries.len() < MAX_CREDENTIALS { credentials.entries.push((key.to_owned(), value.to_vec())); }
        else { return Err(HostError::Conflict("extension credentials are limited to 64 entries".into())); }
        credentials.entries.sort_by(|left, right| left.0.cmp(&right.0));
        credentials.revision = credentials.revision.checked_add(1).ok_or_else(|| HostError::Conflict("extension credential revision is exhausted".into()))?;
        self.publish_credentials_unlocked(&credentials)?; Ok(credentials.revision)
    }

    fn credential_remove(&self, observed: u64, key: &str) -> Result<u64, HostError> {
        let lock = self.lock()?; fs2::FileExt::lock_exclusive(&lock).map_err(Self::failure)?;
        let mut credentials = self.read_credentials_unlocked()?;
        if credentials.revision != observed { return Err(HostError::Conflict("extension credentials changed after they were read".into())); }
        credentials.entries.retain(|(current, _)| current != key);
        credentials.revision = credentials.revision.checked_add(1).ok_or_else(|| HostError::Conflict("extension credential revision is exhausted".into()))?;
        self.publish_credentials_unlocked(&credentials)?; Ok(credentials.revision)
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

fn identity(contents: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha2::Sha256::digest(contents);
    let mut identity = String::with_capacity(71);
    identity.push_str("sha256:");
    for byte in digest {
        identity.push(HEX[usize::from(byte >> 4)] as char);
        identity.push(HEX[usize::from(byte & 15)] as char);
    }
    identity
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use hl_extension::port::ExtensionStateStore as _;

    use super::{MAX_STATE_BYTES, StateBlob};

    fn blob(root: &Path, name: &str) -> StateBlob {
        StateBlob::new(root, &hl_extension::ExtensionName::new(name).unwrap()).unwrap()
    }

    #[test]
    fn state_is_private_durable_bounded_and_idempotently_clearable() {
        let root = tempfile::tempdir().unwrap();
        let state = blob(root.path(), "postgres");
        let empty = state.read().unwrap();
        assert_eq!(empty.contents, Vec::<u8>::new());
        let identity = state.write(&empty.identity, b"{\"schema\":2,\"cursor\":91}").unwrap();
        drop(state);

        let reopened = blob(root.path(), "postgres");
        assert_eq!(reopened.read().unwrap().contents, b"{\"schema\":2,\"cursor\":91}");
        let path = root.path().join("extensions/state/postgres.state");
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        assert!(reopened.write(&identity, &vec![0; MAX_STATE_BYTES + 1]).is_err());
        reopened.clear(&identity).unwrap();
        reopened.clear("absent").unwrap();
        assert!(reopened.read().unwrap().contents.is_empty());
    }

    #[test]
    fn credentials_are_private_durable_cas_safe_and_extension_isolated() {
        let root = tempfile::tempdir().unwrap();
        let postgres = blob(root.path(), "postgres");
        let initial = postgres.credential("password").unwrap();
        assert_eq!(initial.revision, 0); assert_eq!(initial.value, None);
        let revision = postgres.credential_set(0, "password", b"s3cret\0bytes").unwrap();
        assert!(postgres.credential_set(0, "password", b"stale").is_err());
        drop(postgres);

        let reopened = blob(root.path(), "postgres");
        assert_eq!(reopened.credential("password").unwrap().value.as_deref(), Some(b"s3cret\0bytes".as_slice()));
        assert_eq!(blob(root.path(), "other").credential("password").unwrap().value, None);
        let path = root.path().join("extensions/state/postgres.credentials");
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        let removed = reopened.credential_remove(revision, "password").unwrap();
        assert_eq!(reopened.credential("password").unwrap(), hl_extension::port::ExtensionCredential { revision: removed, value: None });
    }

    #[test]
    fn extension_identity_isolates_state_and_symlink_targets_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let postgres = blob(root.path(), "postgres");
        let indexer = blob(root.path(), "indexer");
        postgres.write("absent", b"credential-reference").unwrap();
        indexer.write("absent", b"checkpoint").unwrap();
        assert_eq!(postgres.read().unwrap().contents, b"credential-reference");
        assert_eq!(indexer.read().unwrap().contents, b"checkpoint");

        let identity = postgres.read().unwrap().identity;
        postgres.clear(&identity).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"outside").unwrap();
        symlink(outside.path(), root.path().join("extensions/state/postgres.state")).unwrap();
        assert!(postgres.read().is_err());
        assert!(postgres.write("absent", b"replacement").is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside");
    }

    #[test]
    fn concurrent_instances_reject_a_stale_checkpoint_without_losing_the_winner() {
        let root = tempfile::tempdir().unwrap();
        let foreground = blob(root.path(), "indexer");
        let background = blob(root.path(), "indexer");
        let foreground_read = foreground.read().unwrap();
        let background_read = background.read().unwrap();

        background.write(&background_read.identity, b"cursor=200").unwrap();
        assert!(foreground.write(&foreground_read.identity, b"cursor=100").is_err());
        assert_eq!(foreground.read().unwrap().contents, b"cursor=200");
    }
}
