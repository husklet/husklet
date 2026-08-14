use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    Descriptor, Digest, Error, Image, Reference, Result,
    content::{FsStore, Store},
    copy_graph,
    error::At as _,
    remote::{BlobStream, Source},
    transfer::{CopyReport, Target},
};

const LAYOUT_VERSION: &str = "1.0.0";
const REF_NAME: &str = "org.opencontainers.image.ref.name";

/// A validated OCI image-layout directory usable as a streaming graph source and target.
#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    #[cfg(unix)]
    fn directory(&self, relative: &Path) -> Result<std::os::fd::OwnedFd> {
        use nix::fcntl::{OFlag, open, openat};
        use nix::sys::stat::Mode;

        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
        let mut directory = open(&self.root, flags, Mode::empty()).map_err(std::io::Error::from)?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(Error::InvalidMetadata("layout directory is not relative".into()));
            };
            directory = openat(&directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?;
        }
        Ok(directory)
    }

    /// Create or open an OCI layout.
    ///
    /// # Errors
    /// Returns an error when layout metadata is invalid or cannot be persisted.
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_owned();
        tokio::fs::create_dir_all(root.join("blobs/sha256"))
            .await
            .at(root.join("blobs/sha256"))?;
        let layout = Self { root };
        let marker = layout.root.join("oci-layout");
        if marker.exists() {
            let value: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&marker).await.at(&marker)?)?;
            if value.get("imageLayoutVersion").and_then(serde_json::Value::as_str) != Some(LAYOUT_VERSION) {
                return Err(Error::MalformedOci("unsupported OCI layout version".into()));
            }
        } else {
            layout.publish(&marker, br#"{"imageLayoutVersion":"1.0.0"}"#).await?;
        }
        let index = layout.root.join("index.json");
        if !index.exists() {
            layout
                .publish(
                    &index,
                    br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#,
                )
                .await?;
        }
        Ok(layout)
    }

    /// Export an image's complete descriptor graph and publish its reference in `index.json`.
    ///
    /// # Errors
    /// Returns an error for corrupt local content or an atomic layout update failure.
    pub async fn export(&self, image: &Image, content: &FsStore) -> Result<CopyReport> {
        let source = StoreSource {
            root: image.target.clone(),
            content: content.clone(),
        };
        let report = copy_graph(&source, self, &image.name, image.target.clone()).await?;
        let mut index: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(self.root.join("index.json"))
                .await
                .at(self.root.join("index.json"))?,
        )?;
        let manifests = index
            .get_mut("manifests")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| Error::MalformedOci("layout index has no manifests".into()))?;
        let mut entry = serde_json::to_value(&image.target)?;
        entry
            .as_object_mut()
            .ok_or_else(|| Error::MalformedOci("descriptor is not an object".into()))?
            .insert(
                "annotations".into(),
                serde_json::json!({REF_NAME: image.name.to_string()}),
            );
        manifests.retain(|existing| {
            existing
                .get("annotations")
                .and_then(|v| v.get(REF_NAME))
                .and_then(serde_json::Value::as_str)
                != Some(&image.name.to_string())
        });
        manifests.push(entry);
        self.publish(&self.root.join("index.json"), &serde_json::to_vec(&index)?)
            .await?;
        Ok(report)
    }

    async fn stream(path: PathBuf) -> Result<BlobStream> {
        let file = tokio::fs::File::open(&path).await.at(&path)?;
        Ok(Box::pin(stream::unfold(file, |mut file| async move {
            let mut buffer = vec![0_u8; 64 * 1024];
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(count) => {
                    buffer.truncate(count);
                    Some((Ok(Bytes::from(buffer)), file))
                }
                Err(error) => Some((Err(error.into()), file)),
            }
        })))
    }

    async fn publish(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| parent.starts_with(&self.root))
            .ok_or_else(|| Error::InvalidMetadata("layout path has no owned parent".into()))?;
        #[cfg(unix)]
        {
            use nix::fcntl::renameat;
            use std::os::fd::AsFd as _;
            let relative_parent = parent
                .strip_prefix(&self.root)
                .map_err(|_| Error::InvalidMetadata("layout parent escaped root".into()))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::InvalidMetadata("layout filename is invalid".into()))?;
            let directory = self.directory(relative_parent)?;
            let temporary = format!(".tmp-{}", uuid::Uuid::new_v4());
            let descriptor = nix::fcntl::openat(
                &directory,
                temporary.as_str(),
                nix::fcntl::OFlag::O_WRONLY
                    | nix::fcntl::OFlag::O_CREAT
                    | nix::fcntl::OFlag::O_EXCL
                    | nix::fcntl::OFlag::O_CLOEXEC
                    | nix::fcntl::OFlag::O_NOFOLLOW,
                nix::sys::stat::Mode::from_bits_truncate(0o600),
            )
            .map_err(std::io::Error::from)?;
            let mut file = tokio::fs::File::from_std(std::fs::File::from(descriptor));
            let result = async {
                file.write_all(bytes).await?;
                file.sync_all().await?;
                drop(file);
                renameat(&directory, temporary.as_str(), &directory, name).map_err(std::io::Error::from)?;
                nix::unistd::fsync(directory.as_fd()).map_err(std::io::Error::from)?;
                Ok::<(), Error>(())
            }
            .await;
            if result.is_err() {
                let _ = nix::unistd::unlinkat(&directory, temporary.as_str(), nix::unistd::UnlinkatFlags::NoRemoveDir);
            }
            result
        }
        #[cfg(not(unix))]
        let temporary = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        #[cfg(not(unix))]
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .await
                .at(&temporary)?;
            file.write_all(bytes).await.at(&temporary)?;
            file.sync_all().await.at(&temporary)?;
            drop(file);
            tokio::fs::rename(&temporary, path).await.at(path)?;
            tokio::fs::File::open(parent)
                .await
                .at(parent)?
                .sync_all()
                .await
                .at(parent)?;
            Ok(())
        }
        .await;
        #[cfg(not(unix))]
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        #[cfg(not(unix))]
        return result;
    }
}

#[async_trait]
impl Source for Layout {
    async fn resolve(&self, reference: &Reference) -> Result<Descriptor> {
        let bytes = tokio::fs::read(self.root.join("index.json"))
            .await
            .at(self.root.join("index.json"))?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::MalformedOci("layout index exceeds 16 MiB".into()));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let manifests = value
            .get("manifests")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Error::MalformedOci("layout index has no manifests".into()))?;
        let name = reference.to_string();
        let selected = manifests
            .iter()
            .find(|entry| {
                entry
                    .get("annotations")
                    .and_then(|v| v.get(REF_NAME))
                    .and_then(serde_json::Value::as_str)
                    == Some(&name)
            })
            .or_else(|| (manifests.len() == 1).then(|| &manifests[0]))
            .ok_or(Error::ContentNotFound(name))?;
        serde_json::from_value(selected.clone()).map_err(Into::into)
    }

    async fn fetch(&self, _: &Reference, descriptor: &Descriptor) -> Result<BlobStream> {
        let digest: Digest = descriptor.digest().to_string().parse()?;
        let path = self.root.join("blobs/sha256").join(digest.encoded());
        let metadata = tokio::fs::symlink_metadata(&path).await.at(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::InvalidMetadata(format!(
                "layout blob {digest} is not a regular file"
            )));
        }
        Self::stream(path).await
    }
}

#[async_trait]
impl Target for Layout {
    async fn contains(&self, descriptor: &Descriptor) -> Result<bool> {
        let digest: Digest = descriptor.digest().to_string().parse()?;
        match tokio::fs::symlink_metadata(self.root.join("blobs/sha256").join(digest.encoded())).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(Error::InvalidMetadata(format!("layout blob {digest} is a symlink")))
            }
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    async fn push(&self, descriptor: &Descriptor, mut content: BlobStream) -> Result<()> {
        use futures_util::StreamExt;
        let digest: Digest = descriptor.digest().to_string().parse()?;
        #[cfg(not(unix))]
        let target = self.root.join("blobs/sha256").join(digest.encoded());
        #[cfg(unix)]
        let directory = self.directory(Path::new("blobs/sha256"))?;
        #[cfg(unix)]
        let temporary_name = format!(".tmp-{}", uuid::Uuid::new_v4());
        #[cfg(unix)]
        let descriptor_file = nix::fcntl::openat(
            &directory,
            temporary_name.as_str(),
            nix::fcntl::OFlag::O_WRONLY
                | nix::fcntl::OFlag::O_CREAT
                | nix::fcntl::OFlag::O_EXCL
                | nix::fcntl::OFlag::O_CLOEXEC
                | nix::fcntl::OFlag::O_NOFOLLOW,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .map_err(std::io::Error::from)?;
        #[cfg(unix)]
        let mut file = tokio::fs::File::from_std(std::fs::File::from(descriptor_file));
        #[cfg(not(unix))]
        let temporary = self
            .root
            .join("blobs/sha256")
            .join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        #[cfg(not(unix))]
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await?;
        let mut size = 0_u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = content.next().await {
            let chunk = chunk?;
            size = size.checked_add(chunk.len() as u64).ok_or(Error::SizeMismatch {
                expected: descriptor.size(),
                actual: u64::MAX,
            })?;
            if size > descriptor.size() {
                #[cfg(unix)]
                let _ = nix::unistd::unlinkat(
                    &directory,
                    temporary_name.as_str(),
                    nix::unistd::UnlinkatFlags::NoRemoveDir,
                );
                #[cfg(not(unix))]
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(Error::SizeMismatch {
                    expected: descriptor.size(),
                    actual: size,
                });
            }
            hash.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);
        if size != descriptor.size() {
            #[cfg(unix)]
            let _ = nix::unistd::unlinkat(
                &directory,
                temporary_name.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            );
            #[cfg(not(unix))]
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(Error::SizeMismatch {
                expected: descriptor.size(),
                actual: size,
            });
        }
        let actual = Digest::from(<[u8; 32]>::from(hash.finalize()));
        if actual != digest {
            #[cfg(unix)]
            let _ = nix::unistd::unlinkat(
                &directory,
                temporary_name.as_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            );
            #[cfg(not(unix))]
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(Error::DigestMismatch {
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            nix::fcntl::renameat(&directory, temporary_name.as_str(), &directory, digest.encoded())
                .map_err(std::io::Error::from)?;
            nix::unistd::fsync(directory.as_fd()).map_err(std::io::Error::from)?;
            return Ok(());
        }
        #[cfg(not(unix))]
        match tokio::fs::rename(&temporary, &target).await {
            Ok(()) => {}
            Err(_error) if target.exists() => {
                tokio::fs::remove_file(temporary).await?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(not(unix))]
        Ok(())
    }
}

struct StoreSource {
    root: Descriptor,
    content: FsStore,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::os::unix::fs::symlink;

    #[tokio::test]
    async fn push_refuses_a_replaced_blob_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("layout");
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let layout = Layout::open(&root).await.unwrap();
        std::fs::remove_dir(root.join("blobs/sha256")).unwrap();
        symlink(&outside, root.join("blobs/sha256")).unwrap();

        let bytes = Bytes::from_static(b"checkpoint-authority");
        let digest = Digest::from(<[u8; 32]>::from(Sha256::digest(&bytes)));
        let descriptor: Descriptor = serde_json::from_value(serde_json::json!({
            "mediaType": "application/octet-stream",
            "digest": digest.to_string(),
            "size": bytes.len()
        }))
        .unwrap();
        let content: BlobStream = Box::pin(stream::once(async move { Ok(bytes) }));
        assert!(Target::push(&layout, &descriptor, content).await.is_err());
        assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    }
}
#[async_trait]
impl Source for StoreSource {
    async fn resolve(&self, _: &Reference) -> Result<Descriptor> {
        Ok(self.root.clone())
    }
    async fn fetch(&self, _: &Reference, descriptor: &Descriptor) -> Result<BlobStream> {
        let digest: Digest = descriptor.digest().to_string().parse()?;
        if !self.content.contains(&digest)? {
            return Err(Error::ContentNotFound(digest.to_string()));
        }
        Layout::stream(self.content.path(&digest)).await
    }
}
