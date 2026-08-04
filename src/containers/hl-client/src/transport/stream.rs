use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_util::TryStreamExt;
use http::StatusCode;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Body, Frame, Incoming, SizeHint},
    upgrade::Upgraded,
};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::io::ReaderStream;

use crate::{Error, Result};

type BodyError = Box<dyn std::error::Error + Send + Sync>;

pub(super) struct RequestBody(UnsyncBoxBody<Bytes, BodyError>);

impl RequestBody {
    pub(super) fn full(bytes: Bytes) -> Self {
        Self(Full::new(bytes).map_err(|never| match never {}).boxed_unsync())
    }

    pub(super) fn stream<R>(reader: R) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        Self(
            StreamBody::new(
                ReaderStream::new(reader)
                    .map_ok(Frame::data)
                    .map_err(|error| -> BodyError { Box::new(error) }),
            )
            .boxed_unsync(),
        )
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.0).poll_frame(context)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

/// Pull-based response body. At most one HTTP frame is retained per call.
#[derive(Debug)]
pub struct Stream {
    pub(super) status: StatusCode,
    pub(super) headers: http::HeaderMap,
    pub(super) body: Incoming,
    pub(super) frame_limit: usize,
}

impl Stream {
    pub(crate) const fn limit(&self) -> usize {
        self.frame_limit
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    /// Read the next body frame without buffering later frames.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when the peer's body is malformed and
    /// [`Error::ResponseTooLarge`] when one frame exceeds the configured limit.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>> {
        while let Some(frame) = self.body.frame().await {
            let frame = frame?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            if data.len() > self.frame_limit {
                return Err(Error::ResponseTooLarge {
                    limit: self.frame_limit,
                });
            }
            return Ok(Some(data));
        }
        Ok(None)
    }
}

/// Bidirectional byte stream returned by Docker HTTP connection upgrades.
#[derive(Debug)]
pub struct Upgrade(pub(super) TokioIo<Upgraded>);

impl AsyncRead for Upgrade {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buffer)
    }
}

impl AsyncWrite for Upgrade {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
