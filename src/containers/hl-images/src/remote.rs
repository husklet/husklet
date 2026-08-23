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

/// Whether a registry is reached over TLS or over plain HTTP.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Transport {
    Tls,
    Plain,
}

pub struct Registry {
    /// Built on first use, never at construction.
    ///
    /// Constructing an HTTP client loads the host's CA store, and `reqwest::Client::new()` --
    /// which both `oci_client::Client::default()` and `oci_client::Client::new` reach -- answers a
    /// host that has none by **panicking**:
    ///
    /// ```text
    /// Client::new(): reqwest::Error { kind: Builder,
    ///   source: General("No CA certificates were loaded from the system") }
    /// ```
    ///
    /// A `Registry` is constructed eagerly by things that will never fetch: `hl_daemon::Daemon::new`
    /// builds one before the daemon serves its socket, whether or not any image is ever pulled. So
    /// an eager client turned "this host has no CA store" into a startup panic for the whole
    /// daemon -- fatal in a Nix build sandbox, which sets `SSL_CERT_FILE=/no-cert-file.crt`, and
    /// fatal in a distroless or scratch container image, neither of which ships `ca-certificates`.
    /// Deferring it means only an operation that genuinely needs the network pays, and it pays with
    /// [`Error::RegistryClient`] rather than a panic from inside somebody else's crate.
    ///
    /// The cell caches the client rather than rebuilding per call because it owns the bearer-token
    /// cache and the per-registry auth store, which a pull relies on across its manifest and blob
    /// requests.
    client: std::sync::OnceLock<Client>,
    transport: Transport,
    auth: Auth,
}
impl Registry {
    #[must_use]
    pub fn new(auth: Auth) -> Self {
        Self {
            client: std::sync::OnceLock::new(),
            transport: Transport::Tls,
            auth,
        }
    }

    /// Creates a registry client that permits plain HTTP for explicit insecure/local registries.
    #[must_use]
    pub fn insecure(auth: Auth) -> Self {
        Self {
            client: std::sync::OnceLock::new(),
            transport: Transport::Plain,
            auth,
        }
    }

    /// The registry's HTTP client, built on the first operation that needs one.
    ///
    /// # Errors
    /// Returns [`Error::RegistryClient`] when the host cannot supply what a client needs -- in
    /// practice a missing CA store.
    fn client(&self) -> Result<&Client> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }
        let client = Self::connect(self.transport)?;
        // A concurrent first fetch may have won the race; either client is equivalent, and the
        // loser's is dropped rather than replacing a client whose token cache is already warm.
        Ok(self.client.get_or_init(|| client))
    }

    /// Build the client for `transport`.
    ///
    /// **`Transport::Plain` still needs a CA store, and that cannot be fixed from here.** It speaks
    /// only `http://` -- `ClientProtocol::Http` makes every URL oci-client builds plain -- so the
    /// certificate roots it loads are never consulted, yet `reqwest` loads them while building any
    /// client and a host with none cannot produce one. Two levers exist in `reqwest` and
    /// `oci_client::client::ClientConfig` reaches neither usefully:
    ///
    /// * `ClientBuilder::tls_certs_only([])` gives a real TLS stack over an **empty** root store:
    ///   no CA load, and fail-closed on anything TLS. It is what `hl-daemon`'s remote-`ADD` client
    ///   uses for a plain-`http://` build. `ClientConfig` forwards `tls_certs_only` only when its
    ///   vector is non-empty, so the empty case -- the whole point -- is unreachable through it.
    /// * `accept_invalid_certificates` is reachable and would also build without a CA store, but it
    ///   is not "no TLS", it is "TLS that trusts anyone". An `http://` registry that redirected to
    ///   `https://` would then be accepted silently. Needing a CA store is the better failure.
    ///
    /// So this is a limit of oci-client's configuration surface, not an oversight. Closing it means
    /// oci-client accepting a caller-supplied `reqwest::Client`, or exposing an empty-root-store
    /// option; do not re-derive the above before checking whether it has gained one.
    fn connect(transport: Transport) -> Result<Client> {
        let config = oci_client::client::ClientConfig {
            protocol: match transport {
                Transport::Tls => oci_client::client::ClientProtocol::Https,
                Transport::Plain => oci_client::client::ClientProtocol::Http,
            },
            ..Default::default()
        };
        // `Client::new` swallows this failure and falls back to `Client::default()`, which panics
        // for the same reason; `try_from` is the only construction path that reports it.
        Client::try_from(config).map_err(|error| Error::RegistryClient {
            reason: Self::because(&error),
        })
    }

    /// The whole `source` chain of a client-construction failure, joined.
    ///
    /// `OciDistributionError` and `reqwest::Error` both display as bare category words -- the
    /// missing CA store arrives as `builder error` -- and the sentence an operator can act on is
    /// two links down.
    fn because(error: &dyn std::error::Error) -> String {
        let mut reason = error.to_string();
        let mut source = error.source();
        while let Some(inner) = source {
            reason.push_str(": ");
            reason.push_str(&inner.to_string());
            source = inner.source();
        }
        reason
    }

    /// Stream an image's config and layers, then publish its manifest under `target`.
    ///
    /// # Errors
    /// Returns an error for missing/corrupt content, authentication, or registry failures.
    pub async fn push(&self, image: &Image, target: &Reference, content: &FsStore) -> Result<()> {
        let remote = target.remote()?;
        self.authorize_for(&remote, oci_client::RegistryOperation::Push).await?;
        let manifest = content.read_bounded(&image.target, 16 * 1024 * 1024).await?;
        let document: Manifest =
            serde_json::from_slice(&manifest).map_err(|error| Error::MalformedOci(error.to_string()))?;
        for descriptor in std::iter::once(&document.config).chain(&document.layers) {
            let digest: Digest = descriptor.digest().to_string().parse()?;
            Self::verify(content.path(&digest), descriptor).await?;
            let stream = Self::stream(content.path(&digest)).await?;
            self.client()?
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
        self.client()?
            .push_manifest_raw(&remote, manifest, media)
            .await
            .map_err(Self::error)?;
        Ok(())
    }

    /// Take the repository's pull token before any endpoint that needs one.
    ///
    /// Push has always done this; pull relied on `pull_manifest_raw` to do it implicitly, and it
    /// does not report the outcome -- `Client::get_auth_token` discards a failed token exchange
    /// with `.ok()??` and lets the request continue unauthenticated, so a registry that explained
    /// itself at the token endpoint arrives as a bare 401 from the manifest URL with the
    /// explanation gone. Asking here also removes the throwaway manifest fetch the blob path used
    /// to make for this same purpose: a token request instead of a whole manifest download.
    async fn authorize(&self, remote: &oci_client::Reference) -> Result<()> {
        self.authorize_for(remote, oci_client::RegistryOperation::Pull).await
    }

    async fn authorize_for(
        &self,
        remote: &oci_client::Reference,
        operation: oci_client::RegistryOperation,
    ) -> Result<()> {
        self.client()?
            .auth(remote, &self.auth.registry(), operation)
            .await
            .map(drop)
            .map_err(Self::error)
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

    /// Report a registry failure with the status and the registry's own response body.
    ///
    /// `oci_client` 0.17 keeps the body for every failing status except 401: `UnauthorizedError`
    /// is built from the URL alone and the bytes are dropped. That is the one status Docker Hub
    /// uses for both a credential failure and an exhausted anonymous quota, so the variant with no
    /// body is the variant whose body decides what an operator should do. The pull path calls
    /// [`Client::auth`] explicitly for that reason -- see `resolve` -- which turns a refused
    /// authorization into `AuthenticationFailure` carrying the token endpoint's answer verbatim.
    ///
    /// Nothing here classifies that answer. Docker Hub spells the distinction `UNAUTHORIZED`
    /// against `TOOMANYREQUESTS`; other registries do not, and text the operator reads is worth
    /// more than a verdict this function would get wrong.
    fn error(error: oci_client::errors::OciDistributionError) -> Error {
        use oci_client::errors::OciDistributionError as Oci;
        match error {
            Oci::AuthenticationFailure(body) => Error::registry("the registry refused to authorize", Some(body)),
            Oci::ServerError { code, url, message } => {
                Error::registry(format!("HTTP {code} from {url}"), Some(message))
            }
            Oci::RegistryError { envelope, url } => Error::registry(
                format!("HTTP error from {url}"),
                Some(serde_json::to_string(&envelope).unwrap_or_else(|_| envelope.to_string())),
            ),
            Oci::UnauthorizedError { url } => Error::registry(format!("HTTP 401 from {url}"), None),
            other => Error::registry(other.to_string(), None),
        }
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
        self.authorize(&remote).await?;
        let (bytes, digest) = self
            .client()?
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
            self.authorize(&pinned).await?;
            let (bytes, digest) = self
                .client()?
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
        self.authorize(&remote).await?;
        let digest = descriptor.digest().to_string();
        let stream = self
            .client()?
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

    /// A registry that has not been asked for anything has built no HTTP client.
    ///
    /// Building one loads the host CA store and panics where there is none, and `Registry::new` is
    /// reached by callers that will never fetch -- `hl_daemon::Daemon::new` builds one before the
    /// daemon serves its socket. The deployment shape this protects is pinned by
    /// `hl-daemon/tests/absent_ca_store.rs`; this pins the property that test rests on.
    #[test]
    fn a_registry_builds_no_client_until_an_operation_needs_one() {
        assert!(Registry::new(Auth::Anonymous).client.get().is_none());
        assert!(Registry::insecure(Auth::Anonymous).client.get().is_none());
    }

    /// The words that tell an operator what is wrong are not in the outermost error.
    ///
    /// `OciDistributionError` displays a client-construction failure as `builder error` and
    /// `reqwest::Error` as `builder error` again; `No CA certificates were loaded from the system`
    /// is two `source` links down, so a reason that keeps only the head diagnoses nothing.
    #[test]
    fn a_construction_failure_carries_its_whole_source_chain() {
        #[derive(Debug)]
        struct Layer(&'static str, Option<Box<Layer>>);
        impl std::fmt::Display for Layer {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.0)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1
                    .as_deref()
                    .map(|inner| inner as &(dyn std::error::Error + 'static))
            }
        }

        let error = Layer(
            "builder error",
            Some(Box::new(Layer(
                "unexpected error: No CA certificates were loaded from the system",
                None,
            ))),
        );
        assert_eq!(
            Registry::because(&error),
            "builder error: unexpected error: No CA certificates were loaded from the system"
        );
    }

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

    /// The registry's own answer is what tells an operator to add credentials or to wait, so every
    /// `oci_client` failure that still holds one has to hand it over here.
    #[test]
    fn a_registry_refusal_reports_the_status_and_the_registry_own_words() {
        use oci_client::errors::{OciDistributionError as Oci, OciEnvelope};

        let quota = Registry::error(Oci::AuthenticationFailure(
            r#"{"errors":[{"code":"TOOMANYREQUESTS","message":"pull request limit exceeded"}]}"#.into(),
        ))
        .to_string();
        assert!(quota.contains("TOOMANYREQUESTS"), "{quota}");
        assert!(quota.contains("pull request limit exceeded"), "{quota}");

        let credentials = Registry::error(Oci::AuthenticationFailure(
            r#"{"errors":[{"code":"UNAUTHORIZED","message":"authentication required"}]}"#.into(),
        ))
        .to_string();
        assert!(credentials.contains("UNAUTHORIZED"), "{credentials}");
        assert_ne!(quota, credentials, "the same refusal must not read the same either way");

        let server = Registry::error(Oci::ServerError {
            code: 429,
            url: "https://index.docker.io/v2/library/ubuntu/manifests/24.04".into(),
            message: "You have reached your pull rate limit.".into(),
        })
        .to_string();
        assert!(server.contains("HTTP 429"), "{server}");
        assert!(server.contains("You have reached your pull rate limit."), "{server}");

        let envelope: OciEnvelope = serde_json::from_str(
            r#"{"errors":[{"code":"DENIED","message":"requested access to the resource is denied"}]}"#,
        )
        .unwrap();
        let denied = Registry::error(Oci::RegistryError {
            envelope,
            url: "https://index.docker.io/v2/private/app/manifests/1".into(),
        })
        .to_string();
        assert!(denied.contains("DENIED"), "{denied}");
        assert!(
            denied.contains("requested access to the resource is denied"),
            "{denied}"
        );

        // `oci_client` 0.17 drops the body for exactly this status, so there is nothing to carry
        // and the message says so by naming the status and the endpoint and nothing else.
        let unauthorized = Registry::error(Oci::UnauthorizedError {
            url: "https://index.docker.io/v2/library/ubuntu/manifests/24.04".into(),
        })
        .to_string();
        assert_eq!(
            unauthorized,
            "registry operation failed: HTTP 401 from https://index.docker.io/v2/library/ubuntu/manifests/24.04"
        );
    }

    /// A registry controls its own response body, so the error that carries it has to be the place
    /// the length stops -- every consumer downstream renders it into a log line or an API message.
    #[test]
    fn a_hostile_registry_body_is_bounded_before_it_reaches_a_log() {
        let flood = "A".repeat(512 * 1024);
        let rendered =
            Registry::error(oci_client::errors::OciDistributionError::AuthenticationFailure(flood)).to_string();
        assert!(rendered.len() < 4096, "{}", rendered.len());
        assert!(rendered.contains("(524288 bytes, truncated)"), "{rendered}");
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
