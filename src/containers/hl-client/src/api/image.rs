use http::Method;
use std::fmt::Write as _;

use tokio::io::AsyncRead;

#[derive(serde::Deserialize)]
struct BuildRecord {
    #[serde(default)]
    aux: Option<BuildAux>,
}

#[derive(serde::Deserialize)]
struct BuildAux {
    #[serde(rename = "ID")]
    id: String,
}

struct BuildOptions<'a> {
    tag: &'a str,
    dockerfile: Option<&'a str>,
    arguments: &'a std::collections::BTreeMap<String, String>,
    target: Option<&'a str>,
    no_cache: bool,
    network: Option<&'a str>,
}

use crate::model::{
    Distribution, ImageCommit, ImageDelete, ImageHistory, ImageLoad, ImagePrune, ImageSummary,
    InspectImage, Search,
};
use crate::transport::Transport;
use crate::uri::Component;
use crate::{Result, Stream};

/// Typed image metadata operations.
#[derive(Clone, Copy, Debug)]
pub struct Images<'a> {
    transport: &'a Transport,
}

impl<'a> Images<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// Build a tagged image from a classic Docker tar context.
    ///
    /// # Errors
    /// Returns transport, Dockerfile, build-step, or response failures.
    pub async fn build<R>(&self, reader: R, tag: &str, dockerfile: Option<&str>) -> Result<String>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        self.build_with(reader, tag, dockerfile, &std::collections::BTreeMap::new())
            .await
    }

    /// Build with explicitly declared Dockerfile ARG overrides.
    ///
    /// # Errors
    /// Returns transport, Dockerfile, build-step, or response failures.
    pub async fn build_with<R>(
        &self,
        reader: R,
        tag: &str,
        dockerfile: Option<&str>,
        arguments: &std::collections::BTreeMap<String, String>,
    ) -> Result<String>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        self.build_target(reader, tag, dockerfile, arguments, None, false)
            .await
    }

    /// Build through a named Dockerfile target stage.
    ///
    /// # Errors
    /// Returns transport, Dockerfile, build-step, or response failures.
    pub async fn build_target<R>(
        &self,
        reader: R,
        tag: &str,
        dockerfile: Option<&str>,
        arguments: &std::collections::BTreeMap<String, String>,
        target: Option<&str>,
        no_cache: bool,
    ) -> Result<String>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        self.build_configured(
            reader,
            BuildOptions {
                tag,
                dockerfile,
                arguments,
                target,
                no_cache,
                network: None,
            },
        )
        .await
    }

    /// Build an image with an explicit Docker network mode for `RUN` instructions.
    ///
    /// # Errors
    /// Returns transport, Dockerfile, build-step, network-policy, or response failures.
    pub async fn build_with_network<R>(
        &self,
        reader: R,
        tag: &str,
        dockerfile: Option<&str>,
        network: &str,
    ) -> Result<String>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let arguments = std::collections::BTreeMap::new();
        self.build_configured(
            reader,
            BuildOptions {
                tag,
                dockerfile,
                arguments: &arguments,
                target: None,
                no_cache: false,
                network: Some(network),
            },
        )
        .await
    }

    async fn build_configured<R>(&self, reader: R, options: BuildOptions<'_>) -> Result<String>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let dockerfile = options
            .dockerfile
            .map(|value| format!("&dockerfile={}", Component::opaque(value)))
            .unwrap_or_default();
        let arguments = if options.arguments.is_empty() {
            String::new()
        } else {
            format!(
                "&buildargs={}",
                Component::opaque(&serde_json::to_string(options.arguments)?)
            )
        };
        let target = options
            .target
            .map(|value| format!("&target={}", Component::opaque(value)))
            .unwrap_or_default();
        let no_cache = if options.no_cache {
            "&nocache=true"
        } else {
            ""
        };
        let network = options
            .network
            .map(|value| format!("&networkmode={}", Component::opaque(value)))
            .unwrap_or_default();
        let bytes = self
            .transport
            .upload_raw(
                &format!(
                    "/build?t={}{}{}{}{}{}",
                    Component::opaque(options.tag),
                    dockerfile,
                    arguments,
                    target,
                    no_cache,
                    network
                ),
                reader,
            )
            .await?;
        let mut id = None;
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let record: BuildRecord = serde_json::from_slice(line)?;
            if let Some(aux) = record.aux {
                id = Some(aux.id);
            }
        }
        id.ok_or_else(|| crate::Error::Protocol("build response has no image ID".into()))
    }

    /// Remove all internal build-cache records and reclaim unreachable content.
    ///
    /// # Errors
    /// Returns transport, storage, or response-decoding failures.
    pub async fn prune_builds(&self) -> Result<crate::model::BuildPrune> {
        self.transport
            .json::<(), crate::model::BuildPrune>(Method::POST, "/build/prune", None)
            .await
    }

    /// Snapshot a container filesystem into a named image.
    ///
    /// # Errors
    /// Returns transport, container-state, archive, or image persistence failures.
    pub async fn commit(
        &self,
        container: &str,
        repository: &str,
        tag: Option<&str>,
        pause: bool,
    ) -> Result<ImageCommit> {
        let tag = tag.map_or_else(String::new, |value| {
            format!("&tag={}", Component::opaque(value))
        });
        self.transport
            .json::<(), ImageCommit>(
                Method::POST,
                &format!(
                    "/commit?container={}&repo={}{}&pause={pause}",
                    Component::opaque(container),
                    Component::opaque(repository),
                    tag
                ),
                None,
            )
            .await
    }

    /// Commit a container with typed image metadata and Docker config changes.
    ///
    /// # Errors
    /// Returns validation, transport, container-state, or image persistence failures.
    pub async fn commit_with(&self, options: &crate::model::CommitOptions) -> Result<ImageCommit> {
        let mut parameters = vec![
            format!("container={}", Component::opaque(&options.container)),
            format!("repo={}", Component::opaque(&options.repo)),
            format!("pause={}", options.pause),
        ];
        if !options.tag.is_empty() {
            parameters.push(format!("tag={}", Component::opaque(&options.tag)));
        }
        if !options.author.is_empty() {
            parameters.push(format!("author={}", Component::opaque(&options.author)));
        }
        if !options.comment.is_empty() {
            parameters.push(format!("comment={}", Component::opaque(&options.comment)));
        }
        parameters.extend(
            options
                .changes
                .iter()
                .map(|change| format!("changes={}", Component::opaque(change))),
        );
        self.transport
            .json::<(), ImageCommit>(
                Method::POST,
                &format!("/commit?{}", parameters.join("&")),
                None,
            )
            .await
    }

    /// List locally named images.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn list(&self) -> Result<Vec<ImageSummary>> {
        self.list_with_shared_size(false).await
    }

    /// List local images and optionally account content shared with another image graph.
    ///
    /// # Errors
    /// Returns transport, Docker API, content-accounting, or response-decoding failures.
    pub async fn list_with_shared_size(&self, shared_size: bool) -> Result<Vec<ImageSummary>> {
        self.transport
            .get_json(&format!("/images/json?shared-size={shared_size}"))
            .await
    }

    /// Search the configured registry catalog.
    ///
    /// # Errors
    /// Returns transport, registry, or response-decoding failures.
    pub async fn search(&self, term: &str, limit: Option<usize>) -> Result<Vec<Search>> {
        let mut path = format!("/images/search?term={}", Component::opaque(term));
        if let Some(limit) = limit {
            write!(path, "&limit={limit}").expect("writing to a String cannot fail");
        }
        self.transport.get_json(&path).await
    }

    /// Inspect an image by name, tag, or immutable manifest digest.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn inspect(&self, name: &str) -> Result<InspectImage> {
        self.transport
            .get_json(&format!("/images/{}/json", Component::opaque(name)))
            .await
    }

    /// Inspect the locally resolved distribution descriptor and platform.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn distribution(&self, name: &str) -> Result<Distribution> {
        self.transport
            .get_json(&format!("/distribution/{}/json", Component::opaque(name)))
            .await
    }

    /// Return the image's OCI build history in Docker wire format.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn history(&self, name: &str) -> Result<Vec<ImageHistory>> {
        self.transport
            .get_json(&format!("/images/{}/history", Component::opaque(name)))
            .await
    }

    /// Add `repository[:tag]` as another name for an image.
    ///
    /// # Errors
    /// Returns transport or Docker API failures, including invalid references.
    pub async fn tag(&self, name: &str, repository: &str, tag: Option<&str>) -> Result<()> {
        let suffix = tag.map_or_else(String::new, |value| {
            format!("&tag={}", Component::opaque(value))
        });
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/images/{}/tag?repo={}{}",
                    Component::opaque(name),
                    Component::opaque(repository),
                    suffix
                ),
            )
            .await
    }

    /// Remove one local image name.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn remove(&self, name: &str) -> Result<Vec<ImageDelete>> {
        self.transport
            .json::<(), Vec<ImageDelete>>(
                Method::DELETE,
                &format!("/images/{}", Component::opaque(name)),
                None,
            )
            .await
    }

    /// Reclaim content that is not reachable from a named image or active container rootfs.
    ///
    /// # Errors
    /// Returns transport, storage, or response-decoding failures.
    pub async fn prune(&self) -> Result<ImagePrune> {
        self.transport
            .json::<(), ImagePrune>(Method::POST, "/images/prune", None)
            .await
    }

    /// Load a Docker save archive without buffering it in the client.
    ///
    /// # Errors
    /// Returns reader, transport, archive-validation, or response-decoding failures.
    pub async fn load<R>(&self, reader: R) -> Result<ImageLoad>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        self.load_with(reader, false).await
    }

    /// Load a Docker save archive, optionally suppressing the daemon's status message.
    ///
    /// # Errors
    /// Returns reader, transport, archive-validation, or response-decoding failures.
    pub async fn load_with<R>(&self, reader: R, quiet: bool) -> Result<ImageLoad>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        self.transport
            .upload(&format!("/images/load?quiet={quiet}"), reader)
            .await
    }

    /// Import an uncompressed root filesystem tar as a single-layer image.
    ///
    /// # Errors
    /// Returns reader, transport, archive-validation, or image persistence failures.
    pub async fn import<R>(
        &self,
        reader: R,
        repository: &str,
        tag: Option<&str>,
    ) -> Result<ImageLoad>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let tag = tag.map_or_else(String::new, |value| {
            format!("&tag={}", Component::opaque(value))
        });
        self.transport
            .upload(
                &format!(
                    "/images/create?fromSrc=-&repo={}{}",
                    Component::opaque(repository),
                    tag
                ),
                reader,
            )
            .await
    }

    /// Stream a deterministic Docker save archive for the selected image names.
    ///
    /// # Errors
    /// Returns transport or Docker API failures. Stream body failures are reported by
    /// [`Stream::next_chunk`](crate::Stream::next_chunk).
    pub async fn save(&self, names: &[&str]) -> Result<Stream> {
        let query = names
            .iter()
            .map(|name| Component::opaque(name).to_string())
            .collect::<Vec<_>>()
            .join(",");
        let path = if query.is_empty() {
            "/images/get".to_owned()
        } else {
            format!("/images/get?names={query}")
        };
        self.transport.stream(Method::GET, &path).await
    }
}
#[path = "image_transfer.rs"]
mod transfer;
pub use transfer::{Pull, Push};
