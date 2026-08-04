use base64::Engine as _;
use bytes::{Buf, BytesMut};
use http::Method;
use std::fmt::Write as _;

use crate::model::{Credentials, PullProgress, PushProgress};
use crate::uri::Component;
use crate::{Result, Stream};

use super::Images;

impl Images<'_> {
    /// Start an anonymous OCI registry pull and consume Docker progress records lazily.
    ///
    /// `platform` uses Docker's `os/architecture[/variant]` syntax. When omitted, the daemon's
    /// configured Linux guest platform is used.
    ///
    /// # Errors
    /// Returns transport or Docker API failures. Registry failures arrive as progress records with
    /// [`PullProgress::error`] populated.
    pub async fn pull(&self, image: &str, tag: Option<&str>, platform: Option<&str>) -> Result<Pull> {
        self.pull_with(image, tag, platform, None).await
    }

    /// Pull an image using optional Docker registry credentials.
    ///
    /// # Errors
    /// Returns credential serialization, transport, or Docker API failures. Registry failures
    /// arrive as progress records without echoing credential values.
    pub async fn pull_with(
        &self,
        image: &str,
        tag: Option<&str>,
        platform: Option<&str>,
        credentials: Option<&Credentials>,
    ) -> Result<Pull> {
        let mut query = vec![format!("fromImage={}", Component::opaque(image))];
        if let Some(tag) = tag {
            query.push(format!("tag={}", Component::opaque(tag)));
        }
        if let Some(platform) = platform {
            query.push(format!("platform={}", Component::opaque(platform)));
        }
        let path = format!("/images/create?{}", query.join("&"));
        let stream = if let Some(credentials) = credentials {
            let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(credentials)?);
            self.transport
                .stream_header(
                    Method::POST,
                    &path,
                    http::header::HeaderName::from_static("x-registry-auth"),
                    http::header::HeaderValue::from_str(&encoded)
                        .map_err(|error| crate::Error::Config(error.to_string()))?,
                )
                .await?
        } else {
            self.transport.stream(Method::POST, &path).await?
        };
        Ok(Pull {
            stream,
            buffered: BytesMut::new(),
            finished: false,
        })
    }

    /// Push a local image to its registry and consume Docker progress records lazily.
    ///
    /// # Errors
    /// Returns transport or header-encoding failures. Registry failures arrive as progress records.
    pub async fn push(&self, image: &str, tag: Option<&str>, credentials: Option<&Credentials>) -> Result<Push> {
        let mut path = format!("/images/{}/push", Component::opaque(image));
        if let Some(tag) = tag {
            write!(path, "?tag={}", Component::opaque(tag)).expect("writing to a String cannot fail");
        }
        let stream = if let Some(credentials) = credentials {
            let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(credentials)?);
            self.transport
                .stream_header(
                    Method::POST,
                    &path,
                    http::header::HeaderName::from_static("x-registry-auth"),
                    http::header::HeaderValue::from_str(&encoded)
                        .map_err(|error| crate::Error::Config(error.to_string()))?,
                )
                .await?
        } else {
            self.transport.stream(Method::POST, &path).await?
        };
        Ok(Push {
            stream,
            buffered: BytesMut::new(),
            finished: false,
        })
    }
}

/// Pull-based Docker registry push progress stream.
#[derive(Debug)]
pub struct Push {
    stream: Stream,
    buffered: BytesMut,
    finished: bool,
}

impl Push {
    fn line_end(&mut self) -> Option<usize> {
        while self.buffered.first() == Some(&b'\n') {
            self.buffered.advance(1);
        }
        self.buffered.iter().position(|byte| *byte == b'\n')
    }

    fn take(&mut self, end: usize) -> Result<PushProgress> {
        let mut line = self.buffered.split_to(end + 1);
        line.truncate(end);
        serde_json::from_slice(&line).map_err(Into::into)
    }

    fn finish(&mut self) -> Result<Option<PushProgress>> {
        if self.buffered.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&self.buffered.split())
            .map(Some)
            .map_err(Into::into)
    }

    /// Read one newline-delimited push progress record.
    ///
    /// # Errors
    /// Returns transport failures or malformed progress JSON.
    pub async fn next(&mut self) -> Result<Option<PushProgress>> {
        loop {
            if let Some(index) = self.line_end() {
                return self.take(index).map(Some);
            }
            if self.finished {
                return self.finish();
            }
            match self.stream.next_chunk().await? {
                Some(chunk) => self.buffered.extend_from_slice(&chunk),
                None => self.finished = true,
            }
        }
    }
}

/// Pull-based Docker registry progress stream.
#[derive(Debug)]
pub struct Pull {
    stream: Stream,
    buffered: BytesMut,
    finished: bool,
}

impl Pull {
    fn line_end(&mut self) -> Option<usize> {
        while self.buffered.first() == Some(&b'\n') {
            self.buffered.advance(1);
        }
        self.buffered.iter().position(|byte| *byte == b'\n')
    }

    fn take(&mut self, end: usize) -> Result<PullProgress> {
        let mut line = self.buffered.split_to(end + 1);
        line.truncate(end);
        serde_json::from_slice(&line).map_err(Into::into)
    }

    fn finish(&mut self) -> Result<Option<PullProgress>> {
        if self.buffered.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&self.buffered.split())
            .map(Some)
            .map_err(Into::into)
    }

    /// Read one newline-delimited progress record.
    ///
    /// # Errors
    /// Returns transport failures or malformed progress JSON.
    pub async fn next(&mut self) -> Result<Option<PullProgress>> {
        loop {
            if let Some(index) = self.line_end() {
                return self.take(index).map(Some);
            }
            if self.finished {
                return self.finish();
            }
            match self.stream.next_chunk().await? {
                Some(chunk) => self.buffered.extend_from_slice(&chunk),
                None => self.finished = true,
            }
        }
    }
}
