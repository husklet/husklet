use fs2::FileExt as _;
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::PermissionsExt as _,
    path::{Component, Path, PathBuf},
};

type Error = Box<dyn std::error::Error>;

/// Repository-local storage for immutable testing artifacts and their receipts.
pub(crate) struct Cache {
    root: PathBuf,
}

impl Cache {
    pub(crate) fn new(workspace: &Path) -> Result<Self, Error> {
        if !workspace.is_absolute() {
            return Err("testing workspace root must be absolute".into());
        }
        Ok(Self {
            root: workspace.join(".cache/testing"),
        })
    }

    /// Selects a typed receipt namespace without accepting a caller-provided path.
    pub(crate) fn receipts(&self, namespace: ReceiptNamespace) -> Receipts {
        let relative = match namespace {
            ReceiptNamespace::Nested => PathBuf::from("nested"),
            ReceiptNamespace::Provider(provider) => PathBuf::from("providers").join(provider.name()),
        };
        Receipts {
            root: self.root.join(relative),
            store: self.root.join("store"),
        }
    }
}

/// Owners whose receipts have distinct provenance and reuse policy.
#[derive(Clone, Copy)]
pub(crate) enum ReceiptNamespace {
    Nested,
    #[allow(dead_code, reason = "provider receipts precede the provider pipeline stages")]
    Provider(Provider),
}

#[allow(dead_code, reason = "provider receipts precede the provider pipeline stages")]
#[derive(Clone, Copy)]
pub(crate) enum Provider {
    Engine,
    Docker,
    Qemu,
    Host,
}

impl Provider {
    const fn name(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Docker => "docker",
            Self::Qemu => "qemu",
            Self::Host => "host",
        }
    }
}

/// A SHA-256 identity whose fields cannot collide through concatenation.
pub(crate) struct FramedIdentity(Sha256);

impl FramedIdentity {
    pub(crate) fn new(domain: &[u8]) -> Result<Self, Error> {
        let mut identity = Self(Sha256::new());
        identity.field(domain)?;
        Ok(identity)
    }

    pub(crate) fn field(&mut self, value: &[u8]) -> Result<(), Error> {
        self.0.update(u64::try_from(value.len())?.to_be_bytes());
        self.0.update(value);
        Ok(())
    }

    pub(crate) fn finish(self) -> String {
        hex(self.0.finalize().as_ref())
    }
}

pub(crate) struct Receipts {
    root: PathBuf,
    store: PathBuf,
}

impl Receipts {
    pub(crate) fn artifact(&self, key: &str, file: &str) -> Result<ArtifactRecord, Error> {
        validate_digest("record key", key)?;
        validate_segment("artifact file", file)?;
        let directory = self.root.join("artifacts").join(key);
        Ok(ArtifactRecord {
            artifact: directory.join(file),
            receipt: directory.join("sha256"),
            directory,
            key: key.to_owned(),
            store: self.store.clone(),
        })
    }

    pub(crate) fn lock(&self, key: &str) -> Result<RecordLock, Error> {
        validate_digest("lock key", key)?;
        let directory = self.root.join("locks");
        fs::create_dir_all(&directory)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(directory.join(format!("{key}.lock")))?;
        file.lock_exclusive()?;
        Ok(RecordLock(file))
    }
}

pub(crate) struct RecordLock(File);

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) struct ArtifactRecord {
    directory: PathBuf,
    artifact: PathBuf,
    receipt: PathBuf,
    key: String,
    store: PathBuf,
}

impl ArtifactRecord {
    pub(crate) fn artifact(&self) -> &Path {
        &self.artifact
    }

    pub(crate) fn verify(&self) -> Result<bool, Error> {
        if !self.artifact.is_file() || !self.receipt.is_file() {
            return Ok(false);
        }
        let expected = receipt(&self.key, &fs::read(&self.artifact)?);
        Ok(fs::read_to_string(&self.receipt)?.trim() == expected.trim())
    }

    /// Publishes a complete record by one directory rename while its key lock is held.
    pub(crate) fn publish(&self, bytes: &[u8], executable: bool) -> Result<(), Error> {
        if self.verify()? {
            if fs::read(&self.artifact)? == bytes {
                return Ok(());
            }
            return Err("testing record key already identifies different content".into());
        }
        let parent = self.directory.parent().ok_or("testing record has no parent")?;
        let digest = sha256(bytes);
        let stored = self.publish_content(bytes, &digest)?;
        fs::create_dir_all(parent)?;
        let stage = tempfile::tempdir_in(parent)?;
        let artifact = stage
            .path()
            .join(self.artifact.file_name().ok_or("testing artifact has no file name")?);
        if !executable || fs::hard_link(&stored, &artifact).is_err() {
            fs::copy(&stored, &artifact)?;
        }
        if !executable {
            fs::set_permissions(&artifact, fs::Permissions::from_mode(0o444))?;
        }
        fs::write(stage.path().join("sha256"), receipt_with_digest(&self.key, &digest))?;

        if self.directory.exists() {
            // The caller holds the identity lock. An invalid partial or corrupt
            // record is never observed as the replacement: rename publishes the
            // newly verified directory only after the old entry is removed.
            fs::remove_dir_all(&self.directory)?;
        }
        fs::rename(stage.keep(), &self.directory)?;
        if self.verify()? {
            Ok(())
        } else {
            Err("published testing record failed digest verification".into())
        }
    }

    fn publish_content(&self, bytes: &[u8], digest: &str) -> Result<PathBuf, Error> {
        let directory = self.store.join("artifacts/sha256");
        let locks = self.store.join("locks");
        fs::create_dir_all(&directory)?;
        fs::create_dir_all(&locks)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(locks.join(format!("{digest}.lock")))?;
        lock.lock_exclusive()?;
        let destination = directory.join(digest);
        if destination.is_file() && sha256(&fs::read(&destination)?) == digest {
            return Ok(destination);
        }

        let temporary = tempfile::NamedTempFile::new_in(&directory)?;
        fs::write(temporary.path(), bytes)?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o555))?;
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                if sha256(&fs::read(&destination)?) != digest {
                    return Err("testing object digest changed during publication".into());
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        Ok(destination)
    }
}

fn receipt(key: &str, bytes: &[u8]) -> String {
    receipt_with_digest(key, &sha256(bytes))
}

fn receipt_with_digest(key: &str, digest: &str) -> String {
    format!("key={key}\nsha256={digest}\n")
}

fn validate_segment(label: &str, value: &str) -> Result<(), Error> {
    let path = Path::new(value);
    if value.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || value == "."
        || value == ".."
    {
        return Err(format!("invalid testing {label} {value:?}").into());
    }
    Ok(())
}

fn validate_digest(label: &str, value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid testing {label} {value:?}").into());
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &[u8]) -> String {
        sha256(value)
    }

    #[test]
    fn cache_rejects_paths_that_can_escape_the_testing_root() {
        assert!(Cache::new(Path::new("relative")).is_err());
        let cache = Cache::new(Path::new("/workspace")).unwrap();
        let receipts = cache.receipts(ReceiptNamespace::Nested);
        assert!(receipts.artifact(&key(b"recipe"), "hl-engine").is_ok());
        assert!(receipts.artifact("not-a-digest", "hl-engine").is_err());
        assert!(receipts.artifact(&key(b"recipe"), "../engine").is_err());
    }

    #[test]
    fn framed_fields_do_not_alias_concatenated_values() {
        let mut left = FramedIdentity::new(b"domain").unwrap();
        left.field(b"ab").unwrap();
        left.field(b"c").unwrap();
        let mut right = FramedIdentity::new(b"domain").unwrap();
        right.field(b"a").unwrap();
        right.field(b"bc").unwrap();
        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn publication_is_verified_and_repairable_under_the_key_lock() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = Cache::new(workspace.path()).unwrap();
        let receipts = cache.receipts(ReceiptNamespace::Nested);
        let identity = key(b"recipe");
        let record = receipts.artifact(&identity, "hl-engine").unwrap();
        let _lock = receipts.lock(&identity).unwrap();
        record.publish(b"first", true).unwrap();
        assert!(record.verify().unwrap());
        assert_eq!(fs::read(record.artifact()).unwrap(), b"first");
        assert_ne!(fs::metadata(record.artifact()).unwrap().permissions().mode() & 0o111, 0);
        let stored = workspace
            .path()
            .join(".cache/testing/store/artifacts/sha256")
            .join(sha256(b"first"));
        assert_eq!(fs::read(&stored).unwrap(), b"first");
        assert_eq!(fs::metadata(&stored).unwrap().permissions().mode() & 0o222, 0);

        fs::write(&record.receipt, b"corrupt receipt").unwrap();
        assert!(!record.verify().unwrap());
        record.publish(b"second", true).unwrap();
        assert!(record.verify().unwrap());
        assert_eq!(fs::read(record.artifact()).unwrap(), b"second");
    }

    #[test]
    fn corrupt_content_object_is_not_reused() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = Cache::new(workspace.path()).unwrap();
        let receipts = cache.receipts(ReceiptNamespace::Nested);
        let identity = key(b"recipe");
        let record = receipts.artifact(&identity, "program").unwrap();
        let _lock = receipts.lock(&identity).unwrap();
        record.publish(b"expected", true).unwrap();
        let stored = workspace
            .path()
            .join(".cache/testing/store/artifacts/sha256")
            .join(sha256(b"expected"));
        fs::set_permissions(&stored, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&stored, b"corrupt").unwrap();
        assert!(!record.verify().unwrap());
        record.publish(b"expected", true).unwrap();
        assert!(record.verify().unwrap());
        assert_eq!(fs::read(stored).unwrap(), b"expected");
    }

    #[test]
    fn provider_namespaces_do_not_share_receipts() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = Cache::new(workspace.path()).unwrap();
        let identity = key(b"same recipe");
        let docker = cache.receipts(ReceiptNamespace::Provider(Provider::Docker));
        let qemu = cache.receipts(ReceiptNamespace::Provider(Provider::Qemu));
        let docker_record = docker.artifact(&identity, "program").unwrap();
        let qemu_record = qemu.artifact(&identity, "program").unwrap();
        let _lock = docker.lock(&identity).unwrap();
        docker_record.publish(b"docker", false).unwrap();
        assert!(docker_record.verify().unwrap());
        assert!(!qemu_record.verify().unwrap());
    }

    #[test]
    fn concurrent_writers_publish_one_complete_idempotent_record() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().to_path_buf();
        let identity = key(b"concurrent recipe");
        let writers = (0..8)
            .map(|_| {
                let root = root.clone();
                let identity = identity.clone();
                std::thread::spawn(move || {
                    let cache = Cache::new(&root).unwrap();
                    let receipts = cache.receipts(ReceiptNamespace::Nested);
                    let record = receipts.artifact(&identity, "program").unwrap();
                    let _lock = receipts.lock(&identity).unwrap();
                    record.publish(b"complete", true).unwrap();
                    assert!(record.verify().unwrap());
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }
        let cache = Cache::new(&root).unwrap();
        let receipts = cache.receipts(ReceiptNamespace::Nested);
        let record = receipts.artifact(&identity, "program").unwrap();
        assert!(record.verify().unwrap());
        assert_eq!(fs::read(record.artifact()).unwrap(), b"complete");
    }

    #[test]
    fn all_provider_receipt_namespaces_are_distinct() {
        let cache = Cache::new(Path::new("/workspace")).unwrap();
        let identity = key(b"recipe");
        let paths = [Provider::Engine, Provider::Docker, Provider::Qemu, Provider::Host].map(|provider| {
            cache
                .receipts(ReceiptNamespace::Provider(provider))
                .artifact(&identity, "program")
                .unwrap()
                .artifact
        });
        assert_eq!(
            paths.iter().collect::<std::collections::BTreeSet<_>>().len(),
            paths.len()
        );
    }

    #[test]
    fn receipt_modes_are_independent_for_identical_content() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = Cache::new(workspace.path()).unwrap();
        let identity = key(b"same recipe");
        let engine = cache.receipts(ReceiptNamespace::Provider(Provider::Engine));
        let host = cache.receipts(ReceiptNamespace::Provider(Provider::Host));
        let executable = engine.artifact(&identity, "program").unwrap();
        let data = host.artifact(&identity, "program").unwrap();
        let _engine_lock = engine.lock(&identity).unwrap();
        executable.publish(b"identical", true).unwrap();
        let _host_lock = host.lock(&identity).unwrap();
        data.publish(b"identical", false).unwrap();
        assert_ne!(
            fs::metadata(executable.artifact()).unwrap().permissions().mode() & 0o111,
            0
        );
        assert_eq!(fs::metadata(data.artifact()).unwrap().permissions().mode() & 0o111, 0);
        let store = workspace.path().join(".cache/testing/store/artifacts/sha256");
        assert_eq!(fs::read_dir(store).unwrap().count(), 1);
    }
}
