use bytes::{Buf, BytesMut};
use http::Method;

use crate::model::{Event, EventQuery};
use crate::transport::{Stream, Transport};
use crate::uri::Component;
use crate::{Error, Result};

/// Docker lifecycle event subscriptions.
pub struct Events<'a> {
    transport: &'a Transport,
}

impl<'a> Events<'a> {
    pub(crate) const fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// Subscribe to replayed and live daemon events.
    ///
    /// # Errors
    /// Returns query serialization, transport, HTTP, or response decoding errors.
    pub async fn subscribe(&self, query: &EventQuery) -> Result<EventStream> {
        let mut parameters = Vec::new();
        if !query.filters.0.is_empty() {
            let filters = serde_json::to_string(&query.filters.0)?;
            parameters.push(format!("filters={}", Component::opaque(&filters)));
        }
        if let Some(since) = query.since {
            parameters.push(format!("since={since}"));
        }
        if let Some(until) = query.until {
            parameters.push(format!("until={until}"));
        }
        let suffix = if parameters.is_empty() {
            String::new()
        } else {
            format!("?{}", parameters.join("&"))
        };
        let stream = self.transport.stream(Method::GET, &format!("/events{suffix}")).await?;
        Ok(EventStream::new(stream))
    }
}

/// Pull-based stream of complete Docker event records.
#[derive(Debug)]
pub struct EventStream {
    stream: Stream,
    buffer: BytesMut,
    limit: usize,
    ended: bool,
}

impl EventStream {
    fn new(stream: Stream) -> Self {
        let limit = stream.limit();
        Self {
            stream,
            buffer: BytesMut::new(),
            limit,
            ended: false,
        }
    }

    fn line_end(&self) -> Option<usize> {
        self.buffer.iter().position(|byte| *byte == b'\n')
    }

    fn take(&mut self, end: usize) -> Result<Event> {
        let event = serde_json::from_slice(&self.buffer[..end])?;
        self.buffer.advance(end + 1);
        Ok(event)
    }

    fn finish(&self) -> Result<Option<Event>> {
        if self.buffer.is_empty() {
            Ok(None)
        } else {
            Err(Error::Protocol("truncated event stream record".into()))
        }
    }

    /// Read the next event independently of HTTP frame boundaries.
    ///
    /// # Errors
    /// Returns transport, malformed JSON, truncated record, or size-limit errors.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if let Some(index) = self.line_end() {
                return self.take(index).map(Some);
            }
            if self.ended {
                return self.finish();
            }
            match self.stream.next_chunk().await? {
                Some(chunk) if self.buffer.len().saturating_add(chunk.len()) <= self.limit => {
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(_) => return Err(Error::ResponseTooLarge { limit: self.limit }),
                None => self.ended = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    use super::*;
    use crate::Config;

    async fn request(stream: &mut UnixStream) {
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
    }

    async fn events(chunks: Vec<Vec<u8>>) -> (tempfile::TempDir, EventStream) {
        let root = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            for chunk in chunks {
                socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&chunk).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        });
        let transport = Transport::new(
            Config::unix(root.path().join("daemon.sock"))
                .timeout(Duration::from_secs(1))
                .response_limit(1024),
        );
        let stream = transport.stream(Method::GET, "/events").await.unwrap();
        (root, EventStream::new(stream))
    }

    #[tokio::test]
    async fn records_cross_http_boundaries_and_share_chunks() {
        let first = br#"{"Type":"container","Action":"create","Actor":{"ID":"one","Attributes":{}},"scope":"local","time":1,"timeNano":1}"#;
        let second = br#"{"Type":"container","Action":"destroy","Actor":{"ID":"one","Attributes":{}},"scope":"local","time":2,"timeNano":2}"#;
        let mut tail = Vec::from(&first[17..]);
        tail.push(b'\n');
        tail.extend_from_slice(second);
        tail.push(b'\n');
        let (_root, mut stream) = events(vec![first[..17].to_vec(), tail]).await;
        assert_eq!(stream.next().await.unwrap().unwrap().action, "create");
        assert_eq!(stream.next().await.unwrap().unwrap().action, "destroy");
        assert!(stream.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn truncated_record_is_a_protocol_error() {
        let (_root, mut stream) = events(vec![br#"{"Type":"container"}"#.to_vec()]).await;
        assert!(matches!(stream.next().await, Err(Error::Protocol(_))));
    }
}
