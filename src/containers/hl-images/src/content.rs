use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use bytes::Bytes;
use flate2::read::GzDecoder;

use crate::{Descriptor, DescriptorGraph, Digest, Error, Result, layer::Layer};

pub(crate) struct AppliedLayer {
    pub(crate) diff_id: Digest,
    pub(crate) diff_size: crate::layer::DiffSize,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub digest: Digest,
    pub size: u64,
    pub created_at: SystemTime,
}

pub struct Reader(File);
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}

pub trait Store: Clone + Send + Sync + 'static {
    /// # Errors
    /// Returns an error when storage cannot be inspected.
    fn contains(&self, digest: &Digest) -> Result<bool>;
    /// # Errors
    /// Returns an error when content is missing, corrupt, or inaccessible.
    fn reader(&self, descriptor: &Descriptor) -> Result<Reader>;
    /// # Errors
    /// Returns an error when a staging file cannot be created.
    fn ingest(&self, reference: impl AsRef<str>) -> Result<Draft>;
    /// # Errors
    /// Returns an error when content is missing or metadata cannot be read.
    fn info(&self, digest: &Digest) -> Result<Info>;
}

/// Filesystem content-addressed storage. Only committed blobs appear under `blobs/`.
#[derive(Clone)]
pub struct FsStore {
    root: PathBuf,
    persistence: Arc<dyn crate::storage::Persistence>,
}

impl std::fmt::Debug for FsStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FsStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl FsStore {
    /// # Errors
    /// Returns an error when the store directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(root, Arc::new(crate::storage::Native))
    }

    /// Open content storage using an explicit durable filesystem implementation.
    ///
    /// # Errors
    /// Returns an error when the store directories cannot be created.
    pub fn open_with(root: impl AsRef<Path>, persistence: Arc<dyn crate::storage::Persistence>) -> Result<Self> {
        let root = root.as_ref().to_owned();
        fs::create_dir_all(root.join("blobs/sha256"))?;
        fs::create_dir_all(root.join("ingest"))?;
        Ok(Self { root, persistence })
    }

    fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.root.join("blobs/sha256").join(digest.encoded())
    }

    pub(crate) fn path(&self, digest: &Digest) -> PathBuf {
        self.blob_path(digest)
    }

    /// # Errors
    /// Returns an error when the content directory is malformed or unreadable.
    pub fn digests(&self) -> Result<Vec<Digest>> {
        let mut digests = Vec::new();
        for entry in fs::read_dir(self.root.join("blobs/sha256"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::InvalidMetadata("non-UTF-8 content filename".into()))?;
                digests.push(format!("sha256:{name}").parse()?);
            }
        }
        digests.sort();
        Ok(digests)
    }

    /// # Errors
    /// Returns an error when content cannot be removed or the directory cannot be synchronized.
    pub fn remove(&self, digest: &Digest) -> Result<bool> {
        self.persistence.remove(&self.blob_path(digest))
    }

    pub(crate) fn read_document(&self, descriptor: &Descriptor) -> Result<Bytes> {
        if descriptor.size() > 16 * 1024 * 1024 {
            return Err(Error::MalformedOci("descriptor document exceeds 16 MiB".into()));
        }
        let mut reader = self.reader(descriptor)?;
        let mut bytes = Vec::with_capacity(usize::try_from(descriptor.size()).unwrap_or(0));
        reader.read_to_end(&mut bytes)?;
        Ok(bytes.into())
    }

    pub(crate) async fn read_bounded(&self, descriptor: &Descriptor, limit: u64) -> Result<Bytes> {
        if descriptor.size() > limit {
            return Err(Error::MalformedOci("manifest exceeds size limit".into()));
        }
        let digest: Digest = descriptor.digest().to_string().parse()?;
        let bytes = tokio::fs::read(self.path(&digest)).await?;
        DescriptorGraph::verify(&bytes, descriptor)?;
        Ok(bytes.into())
    }

    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    pub(crate) fn apply_layer(
        &self,
        descriptor: &Descriptor,
        root: &Path,
        ownerships: &mut crate::snapshot::Ownerships,
        names: &mut crate::snapshot::Names,
    ) -> Result<AppliedLayer> {
        let media = descriptor.media_type().to_string();
        let content = self.reader(descriptor)?;
        let decoded: Box<dyn Read> = if media.ends_with("+gzip") || media.ends_with(".gzip") {
            Box::new(GzDecoder::new(content))
        } else if media.ends_with("+zstd") {
            return Err(Error::MalformedOci("zstd layers are not enabled".into()));
        } else {
            Box::new(content)
        };
        let mut reader = DigestReader {
            inner: decoded,
            hash: Sha256::new(),
        };
        let report = Layer::new(&mut reader).apply_with_metadata(root, ownerships, names)?;
        std::io::copy(&mut reader, &mut std::io::sink())?;
        Ok(AppliedLayer {
            diff_id: Digest::from(<[u8; 32]>::from(reader.hash.finalize())),
            diff_size: report.diff_size,
        })
    }
}

struct DigestReader<R> {
    inner: R,
    hash: Sha256,
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.hash.update(&buffer[..count]);
        Ok(count)
    }
}

impl Store for FsStore {
    fn contains(&self, digest: &Digest) -> Result<bool> {
        match fs::symlink_metadata(self.blob_path(digest)) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(Error::InvalidMetadata(format!("content {digest} is a symlink")))
            }
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn reader(&self, descriptor: &Descriptor) -> Result<Reader> {
        let digest: Digest = descriptor.digest().to_string().parse()?;
        let path = self.blob_path(&digest);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::InvalidMetadata(format!("content {digest} is a symlink")));
        }
        let file = File::open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::ContentNotFound(digest.to_string())
            } else {
                error.into()
            }
        })?;
        let actual = file.metadata()?.len();
        let expected = descriptor.size();
        if actual != expected {
            return Err(Error::SizeMismatch { expected, actual });
        }
        Ok(Reader(file))
    }

    fn ingest(&self, reference: impl AsRef<str>) -> Result<Draft> {
        let name = Draft::name(reference.as_ref());
        let path = self.root.join("ingest").join(name);
        let file = OpenOptions::new().create_new(true).write(true).open(&path)?;
        Ok(Draft {
            store: self.clone(),
            path: Some(path),
            file: Some(file),
            hash: Sha256::new(),
            size: 0,
        })
    }

    fn info(&self, digest: &Digest) -> Result<Info> {
        let metadata = fs::metadata(self.blob_path(digest)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::ContentNotFound(digest.to_string())
            } else {
                error.into()
            }
        })?;
        Ok(Info {
            digest: digest.clone(),
            size: metadata.len(),
            created_at: metadata.created().unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }
}

/// A staged write. Dropping or aborting it cannot make partial data visible.
pub struct Draft {
    store: FsStore,
    path: Option<PathBuf>,
    file: Option<File>,
    hash: Sha256,
    size: u64,
}

impl Draft {
    fn name(reference: &str) -> String {
        let reference: String = reference
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .take(48)
            .collect();
        let reference = if reference.is_empty() {
            "ingest".into()
        } else {
            reference
        };
        format!("{reference}-{}", Uuid::new_v4())
    }

    /// # Errors
    /// Returns an error if the ingest is closed, overflows, or cannot be written.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| Error::InvalidMetadata("ingest is closed".into()))?
            .write_all(bytes)?;
        self.hash.update(bytes);
        self.size = self
            .size
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::InvalidMetadata("content size overflow".into()))?;
        Ok(())
    }

    /// # Errors
    /// Returns an error for size/digest mismatch or an atomic storage failure.
    pub fn commit(mut self, expected: &Descriptor) -> Result<Info> {
        let expected_digest: Digest = expected.digest().to_string().parse()?;
        let expected_size = expected.size();
        if self.size != expected_size {
            return Err(Error::SizeMismatch {
                expected: expected_size,
                actual: self.size,
            });
        }
        let actual = Digest::from(<[u8; 32]>::from(self.hash.clone().finalize()));
        if actual != expected_digest {
            return Err(Error::DigestMismatch {
                expected: expected_digest.to_string(),
                actual: actual.to_string(),
            });
        }
        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::InvalidMetadata("ingest is closed".into()))?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        let source = self
            .path
            .take()
            .ok_or_else(|| Error::InvalidMetadata("ingest has no staged path".into()))?;
        let target = self.store.blob_path(&actual);
        if target.exists() {
            fs::remove_file(source)?;
        } else {
            fs::rename(source, &target)?;
            let directory = target
                .parent()
                .ok_or_else(|| Error::InvalidMetadata("blob path has no parent".into()))?;
            hl_fs::Directory::from(directory).sync()?;
        }
        self.store.info(&actual)
    }

    /// # Errors
    /// Returns an error when the staging file cannot be removed.
    pub fn abort(mut self) -> Result<()> {
        self.file.take();
        if let Some(path) = self.path.take() {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        self.file.take();
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FsStore, Store};
    use crate::{
        Digest, Error,
        snapshot::{Id, Snapshots},
    };

    #[test]
    fn zstd_layer_is_rejected_explicitly_when_support_is_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let store = FsStore::open(temp.path().join("content")).unwrap();
        let bytes = b"zstd payload";
        let descriptor = serde_json::from_value(serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar+zstd",
            "digest": Digest::sha256(bytes).to_string(),
            "size": bytes.len()
        }))
        .unwrap();
        let mut ingest = store.ingest("zstd").unwrap();
        ingest.write(bytes).unwrap();
        ingest.commit(&descriptor).unwrap();
        let snapshots = Snapshots::open(temp.path().join("snapshots")).unwrap();
        let mut draft = snapshots.prepare(Id::new("active").unwrap(), None).unwrap();
        let root = draft.path().to_owned();
        let (ownerships, names) = draft.metadata_mut();
        assert!(matches!(
            store.apply_layer(&descriptor, &root, ownerships, names),
            Err(Error::MalformedOci(message)) if message.contains("zstd layers are not enabled")
        ));
    }
}
