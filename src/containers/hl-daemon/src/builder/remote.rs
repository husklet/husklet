use super::BuildError;
use futures_util::StreamExt as _;
use hl_images::build::{Recipe, Step};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt as _;

#[derive(Debug)]
pub(super) struct RemoteSources {
    _directory: tempfile::TempDir,
    files: std::collections::BTreeMap<String, RemoteFile>,
}

#[derive(Debug)]
pub(super) struct RemoteFile {
    root: PathBuf,
    name: String,
    digest: [u8; 32],
}

impl RemoteSources {
    pub(super) async fn fetch(recipe: &Recipe) -> Result<Self, BuildError> {
        const LIMIT: u64 = 256 * 1024 * 1024;
        let directory = tempfile::tempdir()?;
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        let mut files = std::collections::BTreeMap::new();
        for stage in recipe.stages.iter().take(recipe.selected + 1) {
            for step in &stage.steps {
                let Step::Copy {
                    sources, checksum, ..
                } = step
                else {
                    continue;
                };
                for url in sources.iter().filter(|source| source.is_remote()) {
                    let url = url.as_str();
                    if files.contains_key(url) {
                        continue;
                    }
                    let response = client.get(url).send().await?.error_for_status()?;
                    if response.content_length().is_some_and(|size| size > LIMIT) {
                        return Err(BuildError::Copy(format!(
                            "remote ADD source exceeds {LIMIT} bytes"
                        )));
                    }
                    let parsed = reqwest::Url::parse(url).map_err(|error| {
                        hl_images::Error::MalformedOci(format!("invalid ADD URL: {error}"))
                    })?;
                    let name = parsed
                        .path_segments()
                        .and_then(Iterator::last)
                        .filter(|name| {
                            !name.is_empty()
                                && *name != "."
                                && *name != ".."
                                && !name.contains(['/', '\\'])
                        })
                        .unwrap_or("download")
                        .to_owned();
                    let digest =
                        hl_images::Digest::from(<[u8; 32]>::from(Sha256::digest(url.as_bytes())));
                    let root = directory.path().join(digest.encoded());
                    tokio::fs::create_dir(&root).await?;
                    let mut output = tokio::fs::File::create(root.join(&name)).await?;
                    let mut digest = Sha256::new();
                    let mut size = 0_u64;
                    let mut stream = response.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk?;
                        size = size.saturating_add(chunk.len() as u64);
                        if size > LIMIT {
                            return Err(BuildError::Copy(format!(
                                "remote ADD source exceeds {LIMIT} bytes"
                            )));
                        }
                        digest.update(&chunk);
                        output.write_all(&chunk).await?;
                    }
                    output.flush().await?;
                    let digest: [u8; 32] = digest.finalize().into();
                    if let Some(expected) = checksum
                        .as_deref()
                        .and_then(|value| value.strip_prefix("sha256:"))
                    {
                        if !hl_images::Digest::from(digest)
                            .encoded()
                            .eq_ignore_ascii_case(expected)
                        {
                            return Err(BuildError::Copy(format!(
                                "remote ADD checksum mismatch for {url}"
                            )));
                        }
                    }
                    files.insert(url.to_owned(), RemoteFile { root, name, digest });
                }
            }
        }
        Ok(Self {
            _directory: directory,
            files,
        })
    }

    pub(super) fn get(&self, url: &str) -> Result<&RemoteFile, BuildError> {
        self.files
            .get(url)
            .ok_or_else(|| BuildError::Copy(format!("remote ADD source {url:?} was not fetched")))
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (&str, &[u8; 32])> {
        self.files
            .iter()
            .map(|(url, file)| (url.as_str(), &file.digest))
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
