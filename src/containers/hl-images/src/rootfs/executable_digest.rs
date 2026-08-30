use crate::{Error, Result, error::At as _};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Component, Path, PathBuf},
};

const VERSION: u16 = 1;
const MAX_RECORD_BYTES: u64 = 1024;
const MAX_GUEST_PATH_BYTES: usize = 512;

/// Strong content identity authenticated under one immutable snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableDigest {
    pub snapshot: String,
    pub guest_path: String,
    pub size: u64,
    pub sha256: [u8; 32],
    pub bytes_hashed: u64,
}

/// Internal authority for exact executable bytes owned by an immutable lower snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableDigestAuthority {
    snapshot: String,
    lower: PathBuf,
    records: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    snapshot: String,
    guest_path: String,
    size: u64,
    sha256: String,
}

impl ExecutableDigestAuthority {
    pub(super) fn new(snapshot: &str, lower: PathBuf, records: PathBuf) -> Self {
        Self {
            snapshot: snapshot.into(),
            lower,
            records,
        }
    }

    /// Authenticate `host` only when it is the exact immutable-lower binding of `guest_path`.
    /// Corrupt or stale records are ignored and atomically replaced after hashing.
    pub fn authenticate(&self, guest_path: &Path, host: &Path) -> Result<Option<ExecutableDigest>> {
        let guest = normalize_guest(guest_path)?;
        let expected = self.lower.join(guest.strip_prefix("/").expect("normalized absolute"));
        let expected = match fs::canonicalize(&expected) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let host = match fs::canonicalize(host) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let lower = fs::canonicalize(&self.lower).at(&self.lower)?;
        if host != expected || !host.starts_with(&lower) {
            return Ok(None);
        }
        fs::create_dir_all(&self.records).at(&self.records)?;
        fs::set_permissions(&self.records, fs::Permissions::from_mode(0o700)).at(&self.records)?;
        let key = hex(&Sha256::digest(guest.as_bytes()));
        let record_path = self.records.join(format!("{}-{key}.json", self.snapshot));
        let lock_path = self.records.join(format!("{}-{key}.lock", self.snapshot));
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .at(&lock_path)?;
        lock.lock_exclusive().at(&lock_path)?;
        let metadata = fs::metadata(&host).at(&host)?;
        if let Some(record) = read_record(&record_path) {
            if record.version == VERSION
                && record.snapshot == self.snapshot
                && record.guest_path == guest
                && record.size == metadata.len()
                && decode_digest(&record.sha256).is_some()
            {
                return Ok(Some(ExecutableDigest {
                    snapshot: record.snapshot,
                    guest_path: record.guest_path,
                    size: record.size,
                    sha256: decode_digest(&record.sha256).expect("checked"),
                    bytes_hashed: 0,
                }));
            }
        }
        let before = metadata;
        let mut file = fs::File::open(&host).at(&host)?;
        let mut hash = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).at(&host)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| Error::InvalidMetadata("executable size overflow".into()))?;
        }
        let after = file.metadata().at(&host)?;
        if copied != before.len() || after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Ok(None);
        }
        let digest: [u8; 32] = hash.finalize().into();
        let record = Record {
            version: VERSION,
            snapshot: self.snapshot.clone(),
            guest_path: guest.clone(),
            size: copied,
            sha256: hex(&digest),
        };
        replace_private(&record_path, &serde_json::to_vec(&record)?)?;
        Ok(Some(ExecutableDigest {
            snapshot: self.snapshot.clone(),
            guest_path: guest,
            size: copied,
            sha256: digest,
            bytes_hashed: copied,
        }))
    }
}

fn normalize_guest(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        return Err(Error::InvalidMetadata("executable path is not guest-absolute".into()));
    }
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => out.push(value),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(Error::InvalidMetadata("executable path is noncanonical".into()));
            }
        }
    }
    let value = out
        .to_str()
        .ok_or_else(|| Error::InvalidMetadata("executable path is not UTF-8".into()))?
        .to_owned();
    if value.len() > MAX_GUEST_PATH_BYTES {
        return Err(Error::InvalidMetadata("executable path is too long".into()));
    }
    Ok(value)
}

fn read_record(path: &Path) -> Option<Record> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_RECORD_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_RECORD_BYTES)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

fn replace_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .at(&temp)?;
    file.write_all(bytes).at(&temp)?;
    file.sync_all().at(&temp)?;
    fs::rename(&temp, path).at(path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent).and_then(|file| file.sync_all()).at(parent)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fixture(snapshot: &str) -> (tempfile::TempDir, ExecutableDigestAuthority, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::create_dir(lower.join("bin")).unwrap();
        let host = lower.join("bin/tool");
        fs::write(&host, b"authenticated executable bytes").unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
        let authority = ExecutableDigestAuthority::new(snapshot, lower, root.path().join("metadata"));
        (root, authority, host)
    }

    #[test]
    fn second_process_reuses_snapshot_digest_without_hashing_bytes() {
        let (_root, authority, host) = fixture("chain-one");
        let first = authority.authenticate(Path::new("/bin/tool"), &host).unwrap().unwrap();
        assert_eq!(first.bytes_hashed, first.size);
        let reopened = authority.clone();
        let second = reopened.authenticate(Path::new("/bin/tool"), &host).unwrap().unwrap();
        assert_eq!(second.bytes_hashed, 0);
        assert_eq!(first.sha256, second.sha256);
    }

    #[test]
    fn upper_override_has_no_lower_digest_authority() {
        let (root, authority, _host) = fixture("chain-one");
        let upper = root.path().join("upper-tool");
        fs::write(&upper, b"changed upper bytes").unwrap();
        assert_eq!(authority.authenticate(Path::new("/bin/tool"), &upper).unwrap(), None);
    }

    #[test]
    fn copied_snapshot_identity_cannot_reuse_record() {
        let (root, authority, host) = fixture("chain-one");
        authority.authenticate(Path::new("/bin/tool"), &host).unwrap().unwrap();
        let copied = ExecutableDigestAuthority::new("chain-two", authority.lower.clone(), root.path().join("metadata"));
        assert!(
            copied
                .authenticate(Path::new("/bin/tool"), &host)
                .unwrap()
                .unwrap()
                .bytes_hashed
                > 0
        );
    }

    #[test]
    fn corrupt_record_is_ignored_and_rebuilt() {
        let (_root, authority, host) = fixture("chain-one");
        authority.authenticate(Path::new("/bin/tool"), &host).unwrap().unwrap();
        let record = fs::read_dir(&authority.records)
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().and_then(|value| value.to_str()) == Some("json")).then_some(path)
            })
            .unwrap();
        fs::write(&record, b"{corrupt").unwrap();
        assert!(
            authority
                .authenticate(Path::new("/bin/tool"), &host)
                .unwrap()
                .unwrap()
                .bytes_hashed
                > 0
        );
        assert!(read_record(&record).is_some());
    }

    #[test]
    fn concurrent_writers_publish_one_complete_record() {
        let (_root, authority, host) = fixture("chain-one");
        let authority = Arc::new(authority);
        let workers = (0..8)
            .map(|_| {
                let authority = Arc::clone(&authority);
                let host = host.clone();
                std::thread::spawn(move || authority.authenticate(Path::new("/bin/tool"), &host).unwrap().unwrap())
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.bytes_hashed != 0).count(), 1);
        assert!(results.windows(2).all(|pair| pair[0].sha256 == pair[1].sha256));
    }
}
