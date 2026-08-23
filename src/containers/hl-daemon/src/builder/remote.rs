use super::BuildError;
use futures_util::StreamExt as _;
use hl_images::build::{Recipe, Step};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt as _;

const LIMIT: u64 = 256 * 1024 * 1024;

/// The whole `source` chain of an error, joined.
///
/// `reqwest::Error` displays a client-construction failure as `builder error`;
/// `No CA certificates were loaded from the system` is one link down.
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

#[derive(Debug)]
pub(super) struct RemoteSources {
    /// `None` when the recipe asked for no remote source, so nothing was created to hold one.
    _directory: Option<tempfile::TempDir>,
    files: std::collections::BTreeMap<String, RemoteFile>,
}

#[derive(Debug)]
pub(super) struct RemoteFile {
    root: PathBuf,
    name: String,
    digest: [u8; 32],
}

impl RemoteSources {
    /// Fetch every remote `ADD` source the selected stages name, or nothing at all.
    ///
    /// `Builder::build` calls this on **every** build, so the work here is charged to Dockerfiles
    /// that have no remote source in them at all -- which is nearly all of them. Constructing an
    /// HTTP client loads the host's CA store, so doing it before knowing whether there is anything
    /// to fetch made a host with no CA store unable to build *any* image. That is the same
    /// unconditional-construction defect `hl_images::remote::Registry` carried, one layer down.
    /// The remote list is therefore computed first and an empty one returns before a client, or
    /// even a temporary directory, exists.
    pub(super) async fn fetch(recipe: &Recipe) -> Result<Self, BuildError> {
        let remotes = Self::remotes(recipe);
        if remotes.is_empty() {
            return Ok(Self {
                _directory: None,
                files: std::collections::BTreeMap::new(),
            });
        }
        let directory = tempfile::tempdir()?;
        let client = Self::client(remotes.iter().any(|(url, _)| Self::needs_tls(url)))?;
        let mut files = std::collections::BTreeMap::new();
        for (url, checksum) in remotes {
            if files.contains_key(url) {
                continue;
            }
            let file = Self::download(&client, directory.path(), url, checksum).await?;
            files.insert(url.to_owned(), file);
        }
        Ok(Self {
            _directory: Some(directory),
            files,
        })
    }

    /// Every remote `ADD` source in the stages this build will run, with its declared checksum.
    fn remotes(recipe: &Recipe) -> Vec<(&str, Option<&str>)> {
        recipe
            .stages
            .iter()
            .take(recipe.selected + 1)
            .flat_map(|stage| stage.steps.iter())
            .filter_map(|step| match step {
                Step::Copy { sources, checksum, .. } => Some((sources, checksum)),
                Step::Run { .. } => None,
            })
            .flat_map(|(sources, checksum)| {
                sources
                    .iter()
                    .filter(|source| source.is_remote())
                    .map(move |source| (source.as_str(), checksum.as_deref()))
            })
            .collect()
    }

    /// Whether reaching this source means speaking TLS.
    ///
    /// `hl_images::build::Source::Remote` is exactly `http://` or `https://`, but the test is
    /// written as "anything that is not plain `http://`" so a scheme added later takes the
    /// verifying client rather than the empty-root one by default.
    fn needs_tls(url: &str) -> bool {
        !url.starts_with("http://")
    }

    /// The HTTP client for this build's remote sources.
    ///
    /// When none of them is TLS, the client is built with an explicitly **empty** root store. That
    /// loads no CA store, so a host that has none -- a distroless or scratch image without
    /// `ca-certificates`, a Nix build sandbox, which sets `SSL_CERT_FILE=/no-cert-file.crt` -- can
    /// still `ADD http://...`, which needs no certificate from anybody. Measured on 2026-08-21
    /// against a genuinely valid certificate, such a client answers
    /// `invalid peer certificate: UnknownIssuer` where an ordinary one gets 200, so it is
    /// fail-closed: an `http://` source that redirects to `https://` is refused, not trusted.
    ///
    /// `danger_accept_invalid_certs` also builds without a CA store and is the opposite trade --
    /// it would accept that redirect from anyone. Trusting nothing is the right posture for a
    /// client that is not supposed to speak TLS at all; trusting everything is not.
    fn client(tls: bool) -> Result<reqwest::Client, BuildError> {
        let builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5));
        let builder = if tls {
            builder
        } else {
            builder.tls_certs_only(std::iter::empty())
        };
        builder.build().map_err(|error| BuildError::RemoteClient {
            reason: because(&error),
        })
    }

    async fn download(
        client: &reqwest::Client,
        directory: &std::path::Path,
        url: &str,
        checksum: Option<&str>,
    ) -> Result<RemoteFile, BuildError> {
        let response = client.get(url).send().await?.error_for_status()?;
        if response.content_length().is_some_and(|size| size > LIMIT) {
            return Err(BuildError::Copy(format!("remote ADD source exceeds {LIMIT} bytes")));
        }
        let parsed = reqwest::Url::parse(url)
            .map_err(|error| hl_images::Error::MalformedOci(format!("invalid ADD URL: {error}")))?;
        let name = parsed
            .path_segments()
            .and_then(Iterator::last)
            .filter(|name| !name.is_empty() && *name != "." && *name != ".." && !name.contains(['/', '\\']))
            .unwrap_or("download")
            .to_owned();
        let digest = hl_images::Digest::from(<[u8; 32]>::from(Sha256::digest(url.as_bytes())));
        let root = directory.join(digest.encoded());
        tokio::fs::create_dir(&root).await?;
        let mut output = tokio::fs::File::create(root.join(&name)).await?;
        let mut digest = Sha256::new();
        let mut size = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size = size.saturating_add(chunk.len() as u64);
            if size > LIMIT {
                return Err(BuildError::Copy(format!("remote ADD source exceeds {LIMIT} bytes")));
            }
            digest.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        let digest: [u8; 32] = digest.finalize().into();
        if let Some(expected) = checksum.and_then(|value| value.strip_prefix("sha256:"))
            && !hl_images::Digest::from(digest).encoded().eq_ignore_ascii_case(expected)
        {
            return Err(BuildError::Copy(format!("remote ADD checksum mismatch for {url}")));
        }
        Ok(RemoteFile { root, name, digest })
    }

    pub(super) fn get(&self, url: &str) -> Result<&RemoteFile, BuildError> {
        self.files
            .get(url)
            .ok_or_else(|| BuildError::Copy(format!("remote ADD source {url:?} was not fetched")))
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (&str, &[u8; 32])> {
        self.files.iter().map(|(url, file)| (url.as_str(), &file.digest))
    }
}

impl RemoteFile {
    pub(super) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }
}
