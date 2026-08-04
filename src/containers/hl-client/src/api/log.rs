use bytes::{Buf, BytesMut};

use crate::transport::Stream;
use crate::{Error, Result};

use super::{Channel, Output};

const HEADER: usize = 8;

/// Pull-based, ordered container output from Docker's multiplexed logs endpoint.
#[derive(Debug)]
pub struct LogStream {
    stream: Stream,
    buffer: BytesMut,
    frame_limit: usize,
    ended: bool,
    terminal: bool,
}

impl LogStream {
    pub(crate) fn new(stream: Stream, frame_limit: usize) -> Self {
        Self {
            stream,
            buffer: BytesMut::new(),
            frame_limit,
            ended: false,
            terminal: false,
        }
    }

    pub(crate) fn terminal(stream: Stream, frame_limit: usize) -> Self {
        Self {
            stream,
            buffer: BytesMut::new(),
            frame_limit,
            ended: false,
            terminal: true,
        }
    }

    /// Read the next complete stdout or stderr frame.
    ///
    /// HTTP body boundaries are intentionally invisible to callers: a Docker frame may be split
    /// across any number of body chunks, and one body chunk may contain multiple Docker frames.
    ///
    /// # Errors
    /// Returns a transport or protocol error for malformed, truncated, or oversized frames.
    pub async fn next(&mut self) -> Result<Option<Output>> {
        if self.terminal {
            return self.next_terminal().await;
        }
        if !self.fill(HEADER).await? {
            if self.buffer.is_empty() {
                return Ok(None);
            }
            return Err(Error::Protocol("truncated log stream frame header".into()));
        }

        let channel = match self.buffer[0] {
            1 => Channel::Stdout,
            2 => Channel::Stderr,
            value => return Err(Error::Protocol(format!("invalid log stream identifier {value}"))),
        };
        if self.buffer[1..4] != [0, 0, 0] {
            return Err(Error::Protocol("log stream frame reserved bytes are not zero".into()));
        }
        let length = u32::from_be_bytes([self.buffer[4], self.buffer[5], self.buffer[6], self.buffer[7]]) as usize;
        if length > self.frame_limit {
            return Err(Error::ResponseTooLarge {
                limit: self.frame_limit,
            });
        }
        let complete = HEADER + length;
        if !self.fill(complete).await? {
            return Err(Error::Protocol("truncated log stream frame payload".into()));
        }
        self.buffer.advance(HEADER);
        let bytes = self.buffer.split_to(length).freeze();
        Ok(Some(Output::new(channel, bytes)))
    }

    async fn next_terminal(&mut self) -> Result<Option<Output>> {
        if !self.buffer.is_empty() {
            return Ok(Some(Output::new(Channel::Stdout, self.buffer.split().freeze())));
        }
        match self.stream.next_chunk().await? {
            Some(bytes) => {
                if bytes.len() > self.frame_limit {
                    return Err(Error::ResponseTooLarge {
                        limit: self.frame_limit,
                    });
                }
                Ok(Some(Output::new(Channel::Stdout, bytes)))
            }
            None => Ok(None),
        }
    }

    async fn fill(&mut self, length: usize) -> Result<bool> {
        while self.buffer.len() < length && !self.ended {
            match self.stream.next_chunk().await? {
                Some(chunk) => self.buffer.extend_from_slice(&chunk),
                None => self.ended = true,
            }
        }
        Ok(self.buffer.len() >= length)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    use super::*;
    use crate::model::{LogOptions, LogStreams};
    use crate::transport::Transport;
    use crate::{Client, Config};

    fn frame(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![channel, 0, 0, 0];
        frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    async fn request(stream: &mut UnixStream) {
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
    }

    async fn stream(chunks: Vec<Vec<u8>>, limit: usize, terminal: bool) -> (tempfile::TempDir, LogStream) {
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
                .response_limit(limit),
        );
        let body = transport.stream(http::Method::GET, "/logs").await.unwrap();
        (
            root,
            if terminal {
                LogStream::terminal(body, limit)
            } else {
                LogStream::new(body, limit)
            },
        )
    }

    #[tokio::test]
    async fn frames_cross_http_boundaries_and_share_chunks() {
        let mut bytes = frame(1, b"hello");
        bytes.extend(frame(2, b"error"));
        let chunks = vec![
            bytes[..1].to_vec(),
            bytes[1..6].to_vec(),
            bytes[6..15].to_vec(),
            bytes[15..].to_vec(),
        ];
        let (_root, mut logs) = stream(chunks, 64, false).await;
        let first = logs.next().await.unwrap().unwrap();
        assert_eq!(first.channel(), Channel::Stdout);
        assert_eq!(first.bytes(), b"hello".as_slice());
        let second = logs.next().await.unwrap().unwrap();
        assert_eq!(second.channel(), Channel::Stderr);
        assert_eq!(second.bytes(), b"error".as_slice());
        assert_eq!(logs.next().await.unwrap(), None);
        assert_eq!(logs.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn terminal_logs_are_raw_stdout_without_multiplex_headers() {
        let (_root, mut logs) = stream(vec![b"terminal\n".to_vec()], 64, true).await;
        let output = logs.next().await.unwrap().unwrap();
        assert_eq!(output.channel(), Channel::Stdout);
        assert_eq!(output.bytes().as_ref(), b"terminal\n");
        assert_eq!(logs.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn malformed_and_truncated_frames_are_rejected() {
        let (_root, mut logs) = stream(vec![vec![1, 0, 0]], 64, false).await;
        assert!(matches!(logs.next().await, Err(Error::Protocol(message)) if message.contains("header")));

        let mut reserved = frame(1, b"x");
        reserved[2] = 1;
        let (_root, mut logs) = stream(vec![reserved], 64, false).await;
        assert!(matches!(logs.next().await, Err(Error::Protocol(message)) if message.contains("reserved")));

        let (_root, mut logs) = stream(vec![frame(9, b"x")], 64, false).await;
        assert!(matches!(logs.next().await, Err(Error::Protocol(message)) if message.contains("identifier")));

        let mut payload = frame(1, b"hello");
        payload.truncate(payload.len() - 2);
        let (_root, mut logs) = stream(vec![payload], 64, false).await;
        assert!(matches!(logs.next().await, Err(Error::Protocol(message)) if message.contains("payload")));
    }

    #[tokio::test]
    async fn declared_oversized_frame_is_rejected_before_payload() {
        let mut header = frame(1, &[]);
        header[4..8].copy_from_slice(&65_u32.to_be_bytes());
        let (_root, mut logs) = stream(vec![header], 64, false).await;
        assert!(matches!(logs.next().await, Err(Error::ResponseTooLarge { limit: 64 })));
    }

    #[tokio::test]
    async fn options_are_encoded_as_an_exact_docker_query() {
        let root = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                bytes.push(byte[0]);
            }
            let first = bytes.split(|byte| *byte == b'\r').next().unwrap();
            sent.send(String::from_utf8(first.to_vec()).unwrap()).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let client = Client::unix(root.path().join("daemon.sock")).unwrap();
        let options = LogOptions {
            follow: true,
            streams: LogStreams {
                stdout: false,
                stderr: true,
            },
            since_ms: Some(1_500),
            until_ms: Some(2_001),
            timestamps: true,
            tail: Some(17),
        };
        let mut logs = client.containers().logs_stream("id/unsafe", &options).await.unwrap();
        assert_eq!(logs.next().await.unwrap(), None);
        assert_eq!(
            received.await.unwrap(),
            "GET /v1.43/containers/id%2Funsafe/logs?follow=true&stdout=false&stderr=true&timestamps=true&since=1.500&until=2.001&tail=17 HTTP/1.1"
        );
        server.await.unwrap();
    }
}
