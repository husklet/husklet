use http::Method;

use crate::model::PathStat;
use crate::transport::Stream;
use crate::uri::Component;
use crate::{Error, Result};

use super::Containers;

/// Streaming container filesystem archive and its Docker path metadata.
#[derive(Debug)]
pub struct Archive {
    stat: PathStat,
    stream: Stream,
}

/// Decoded value carried by Docker's container-path metadata header.
#[derive(Debug)]
struct PathStatHeader(PathStat);

impl Archive {
    #[must_use]
    pub fn stat(&self) -> &PathStat {
        &self.stat
    }

    #[must_use]
    pub fn into_stream(self) -> Stream {
        self.stream
    }
}

impl Containers<'_> {
    /// Read Docker path metadata for a container filesystem entry.
    ///
    /// # Errors
    /// Returns transport, lookup, path-validation, or protocol failures.
    pub async fn stat(&self, id: &str, path: &str) -> Result<PathStat> {
        let headers = self
            .transport
            .head(&format!(
                "/containers/{}/archive?path={}",
                Component::opaque(id),
                Component::opaque(path)
            ))
            .await?;
        Ok(PathStatHeader::try_from(&headers)?.into())
    }

    /// Stream a tar archive containing a container path.
    ///
    /// # Errors
    /// Returns transport, lookup, path-validation, or protocol failures.
    pub async fn copy_from(&self, id: &str, path: &str) -> Result<Archive> {
        let stream = self
            .transport
            .stream(
                Method::GET,
                &format!(
                    "/containers/{}/archive?path={}",
                    Component::opaque(id),
                    Component::opaque(path)
                ),
            )
            .await?;
        let stat = PathStatHeader::try_from(stream.headers())?.into();
        Ok(Archive { stat, stream })
    }

    /// Stream the complete container root filesystem as a tar archive.
    ///
    /// # Errors
    /// Returns transport, lookup, filesystem, or protocol failures.
    pub async fn export(&self, id: &str) -> Result<Stream> {
        self.transport
            .stream(
                Method::GET,
                &format!("/containers/{}/export", Component::opaque(id)),
            )
            .await
    }

    /// Stream a tar archive into an existing container directory.
    ///
    /// # Errors
    /// Returns transport, lookup, read-only mount, archive-validation, or filesystem failures.
    pub async fn copy_to<R>(&self, id: &str, path: &str, archive: R) -> Result<()>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.copy_to_owned(id, path, archive, false).await
    }

    /// Stream a tar archive into a container, optionally preserving tar uid/gid metadata.
    ///
    /// # Errors
    /// Returns transport, lookup, archive-validation, ownership, or filesystem failures.
    pub async fn copy_to_owned<R>(
        &self,
        id: &str,
        path: &str,
        archive: R,
        copy_uid_gid: bool,
    ) -> Result<()>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.transport
            .upload_empty(
                Method::PUT,
                &format!(
                    "/containers/{}/archive?path={}&copyUIDGID={copy_uid_gid}",
                    Component::opaque(id),
                    Component::opaque(path)
                ),
                archive,
            )
            .await
    }
}

impl TryFrom<&http::HeaderMap> for PathStatHeader {
    type Error = Error;

    fn try_from(headers: &http::HeaderMap) -> Result<Self> {
        use base64::Engine as _;

        let value = headers
            .get("X-Docker-Container-Path-Stat")
            .ok_or_else(|| Error::Protocol("archive response omitted path metadata".into()))?
            .to_str()
            .map_err(|error| Error::Protocol(error.to_string()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(Self(serde_json::from_slice(&bytes)?))
    }
}

impl From<PathStatHeader> for PathStat {
    fn from(header: PathStatHeader) -> Self {
        header.0
    }
}
