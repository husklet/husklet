use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use oci_client::{Client, secrets::RegistryAuth};
use std::pin::Pin;
use tokio::io::AsyncReadExt;

use crate::{Descriptor, Digest, Error, Image, Reference, Result, content::FsStore, error::At as _};

pub(crate) const MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

#[derive(Clone, Default)]
pub enum Auth {
    #[default]
    Anonymous,
    Basic {
        username: String,
        password: String,
    },
    Bearer(String),
}
impl Auth {
    fn registry(&self) -> RegistryAuth {
        match self {
            Self::Anonymous => RegistryAuth::Anonymous,
            Self::Basic { username, password } => RegistryAuth::Basic(username.clone(), password.clone()),
            Self::Bearer(token) => RegistryAuth::Bearer(token.clone()),
        }
    }
}

/// A content-neutral OCI Distribution source. Local tests/layouts can implement this without a server.
#[async_trait]
pub trait Source: Send + Sync {
    async fn resolve(&self, reference: &Reference) -> Result<Descriptor>;
    async fn fetch(&self, reference: &Reference, descriptor: &Descriptor) -> Result<BlobStream>;
}

pub type BlobStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + 'static>>;

pub struct Registry {
    client: Client,
    auth: Auth,
}
impl Registry {
    #[must_use]
    pub fn new(auth: Auth) -> Self {
        Self {
            client: Client::default(),
            auth,
        }
    }

    /// Creates a registry client that permits plain HTTP for explicit insecure/local registries.
    #[must_use]
    pub fn insecure(auth: Auth) -> Self {
        let config = oci_client::client::ClientConfig {
            protocol: oci_client::client::ClientProtocol::Http,
            ..Default::default()
        };
        Self {
            client: Client::new(config),
            auth,
        }
    }

    /// Stream an image's config and layers, then publish its manifest under `target`.
    ///
    /// # Errors
    /// Returns an error for missing/corrupt content, authentication, or registry failures.
    pub async fn push(&self, image: &Image, target: &Reference, content: &FsStore) -> Result<()> {
        let remote = target.remote()?;
        self.client
            .auth(&remote, &self.auth.registry(), oci_client::RegistryOperation::Push)
            .await
            .map_err(Self::error)?;
        let manifest = content.read_bounded(&image.target, 16 * 1024 * 1024).await?;
        let document: Manifest =
            serde_json::from_slice(&manifest).map_err(|error| Error::MalformedOci(error.to_string()))?;
        for descriptor in std::iter::once(&document.config).chain(&document.layers) {
            let digest: Digest = descriptor.digest().to_string().parse()?;
            Self::verify(content.path(&digest), descriptor).await?;
            let stream = Self::stream(content.path(&digest)).await?;
            self.client
                .push_blob_stream(&remote, stream, descriptor.digest().to_string().as_str())
                .await
                .map_err(Self::error)?;
        }
        let media = image
            .target
            .media_type()
            .to_string()
            .parse()
            .map_err(|error| Error::MalformedOci(format!("invalid manifest media type: {error}")))?;
        self.client
            .push_manifest_raw(&remote, manifest, media)
            .await
            .map_err(Self::error)?;
        Ok(())
    }

    /// List descriptors referring to a subject, using the OCI fallback implemented by `oci-client`.
    ///
    /// # Errors
    /// Returns an error for invalid references, authentication, or registry responses.
    pub async fn referrers(
        &self,
        repository: &Reference,
        subject: &Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<Descriptor>> {
        let reference: oci_client::Reference =
            format!("{}/{}@{subject}", repository.registry(), repository.repository())
                .parse()
                .map_err(|error| Error::InvalidReference(format!("{error}")))?;
        self.client
            .auth(&reference, &self.auth.registry(), oci_client::RegistryOperation::Pull)
            .await
            .map_err(Self::error)?;
        let index = self
            .client
            .pull_referrers(&reference, artifact_type)
            .await
            .map_err(Self::error)?;
        index
            .manifests
            .into_iter()
            .map(|entry| serde_json::from_value(serde_json::to_value(entry)?).map_err(Into::into))
            .collect()
    }

    fn manifest(bytes: &[u8], digest: &str) -> Result<Descriptor> {
        let _: Digest = digest.parse()?;
        let document: serde_json::Value = serde_json::from_slice(bytes)?;
        let media_type = document
            .get("mediaType")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::MalformedOci("manifest has no mediaType".into()))?;
        serde_json::from_value(serde_json::json!({ "mediaType": media_type, "digest": digest, "size": bytes.len() }))
            .map_err(Into::into)
    }

    async fn stream(
        path: std::path::PathBuf,
    ) -> Result<impl futures_util::Stream<Item = oci_client::errors::Result<Bytes>>> {
        let file = tokio::fs::File::open(&path).await.at(&path)?;
        Ok(stream::unfold(file, |mut file| async move {
            let mut buffer = vec![0_u8; 64 * 1024];
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(count) => {
                    buffer.truncate(count);
                    Some((Ok(Bytes::from(buffer)), file))
                }
                Err(error) => Some((Err(error.into()), file)),
            }
        }))
    }

    async fn verify(path: std::path::PathBuf, descriptor: &Descriptor) -> Result<()> {
        use sha2::Digest as _;
        let mut file = tokio::fs::File::open(&path).await.at(&path)?;
        let mut hash = sha2::Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            size = size.checked_add(count as u64).ok_or(Error::SizeMismatch {
                expected: descriptor.size(),
                actual: u64::MAX,
            })?;
            if size > descriptor.size() {
                return Err(Error::SizeMismatch {
                    expected: descriptor.size(),
                    actual: size,
                });
            }
            hash.update(&buffer[..count]);
        }
        if size != descriptor.size() {
            return Err(Error::SizeMismatch {
                expected: descriptor.size(),
                actual: size,
            });
        }
        let actual = Digest::from(<[u8; 32]>::from(hash.finalize()));
        let expected: Digest = descriptor.digest().to_string().parse()?;
        if actual != expected {
            return Err(Error::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    }

    fn error(error: impl std::fmt::Display) -> Error {
        Error::Registry(error.to_string())
    }
}

#[derive(serde::Deserialize)]
struct Manifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[async_trait]
impl Source for Registry {
    async fn resolve(&self, reference: &Reference) -> Result<Descriptor> {
        let remote = reference.remote()?;
        let (bytes, digest) = self
            .client
            .pull_manifest_raw(&remote, &self.auth.registry(), MANIFEST_MEDIA_TYPES)
            .await
            .map_err(Self::error)?;
        Self::manifest(&bytes, &digest)
    }

    async fn fetch(&self, reference: &Reference, descriptor: &Descriptor) -> Result<BlobStream> {
        let media = descriptor.media_type().to_string();
        if MANIFEST_MEDIA_TYPES.contains(&media.as_str()) {
            let pinned: oci_client::Reference = format!(
                "{}/{}@{}",
                reference.registry(),
                reference.repository(),
                descriptor.digest()
            )
            .parse()
            .map_err(|error| Error::InvalidReference(format!("{error}")))?;
            let (bytes, digest) = self
                .client
                .pull_manifest_raw(&pinned, &self.auth.registry(), MANIFEST_MEDIA_TYPES)
                .await
                .map_err(Self::error)?;
            if digest != descriptor.digest().to_string() {
                return Err(Error::DigestMismatch {
                    expected: descriptor.digest().to_string(),
                    actual: digest,
                });
            }
            return Ok(Box::pin(stream::once(async move { Ok(bytes) })));
        }
        let remote = reference.remote()?;
        // Authorize the repository before its blob endpoint is used.
        let _ = self
            .client
            .pull_manifest_raw(&remote, &self.auth.registry(), MANIFEST_MEDIA_TYPES)
            .await
            .map_err(Self::error)?;
        let digest = descriptor.digest().to_string();
        let stream = self
            .client
            .pull_blob_stream(&remote, digest.as_str())
            .await
            .map_err(Self::error)?;
        Ok(Box::pin(stream.map(|item| item.map_err(Into::into))))
    }
}

#[cfg(test)]
mod tests {
    use super::{Auth, Registry};
    use crate::{Descriptor, Digest, Error};
    use futures_util::StreamExt as _;
    use oci_client::secrets::RegistryAuth;

    #[test]
    fn registry_authentication_is_typed_and_does_not_mutate_unrelated_headers() {
        assert_eq!(Auth::Anonymous.registry(), RegistryAuth::Anonymous);
        assert_eq!(
            Auth::Basic {
                username: "user".into(),
                password: "secret".into(),
            }
            .registry(),
            RegistryAuth::Basic("user".into(), "secret".into())
        );
        assert_eq!(
            Auth::Bearer("token".into()).registry(),
            RegistryAuth::Bearer("token".into())
        );
    }

    #[tokio::test]
    async fn registry_upload_stream_is_bounded_and_verifies_content() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("blob");
        let bytes = vec![7_u8; 128 * 1024 + 3];
        tokio::fs::write(&path, &bytes).await.unwrap();
        let descriptor: Descriptor = serde_json::from_value(serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": Digest::sha256(&bytes).to_string(),
            "size": bytes.len()
        }))
        .unwrap();

        Registry::verify(path.clone(), &descriptor).await.unwrap();
        let stream = Registry::stream(path.clone()).await.unwrap();
        futures_util::pin_mut!(stream);
        let mut streamed = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= 64 * 1024);
            streamed.extend_from_slice(&chunk);
        }
        assert_eq!(streamed, bytes);

        tokio::fs::write(&path, b"corrupt").await.unwrap();
        assert!(matches!(
            Registry::verify(path, &descriptor).await,
            Err(Error::SizeMismatch { .. })
        ));
    }
}
