use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use hl_container::{ExecId, Executions, Session, Signal, Streams};
use serde::Deserialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::api::ContainerLogs;

use super::error::{ApiError, ApiResult};

#[derive(Deserialize)]
pub(super) struct Resize {
    #[serde(rename = "h")]
    height: u64,
    #[serde(rename = "w")]
    width: u64,
}

impl Resize {
    pub(super) fn size(self) -> ApiResult<hl_container::Size> {
        let rows = u16::try_from(self.height)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "terminal height exceeds 65535"))?;
        let columns = u16::try_from(self.width)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "terminal width exceeds 65535"))?;
        hl_container::Size::new(rows, columns).map_err(ApiError::container)
    }
}

pub(super) struct Connection {
    upgrade: hyper::upgrade::OnUpgrade,
    session: Session,
    streams: Streams,
    terminal: bool,
    detach: Option<DetachKeys>,
    disconnect: Option<Disconnect>,
}

impl Connection {
    pub(super) fn new(
        upgrade: hyper::upgrade::OnUpgrade,
        session: Session,
        streams: Streams,
        terminal: bool,
    ) -> Self {
        Self {
            upgrade,
            session,
            streams,
            terminal,
            detach: None,
            disconnect: None,
        }
    }

    pub(super) fn kill_on_disconnect(mut self, executions: Executions, id: ExecId) -> Self {
        self.disconnect = Some(Disconnect { executions, id });
        self
    }

    pub(super) fn detach_keys(mut self, value: &str) -> ApiResult<Self> {
        self.detach = DetachKeys::parse(value)?;
        Ok(self)
    }

    pub(super) fn spawn(self) -> Response {
        tokio::spawn(self.run());
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "tcp")
            .header(header::CONTENT_TYPE, "application/vnd.docker.raw-stream")
            .body(Body::empty())
            .expect("static console response is valid")
    }

    async fn run(mut self) {
        let Ok(upgraded) = self.upgrade.await else {
            return;
        };
        let stream = hyper_util::rt::TokioIo::new(upgraded);
        let (mut reader, mut writer) = tokio::io::split(stream);
        let input = self.session.input();
        let (ended, mut end) = tokio::sync::oneshot::channel();
        let input_task = self.streams.stdin.then(|| {
            let mut keys = self.detach.map(DetachInput::new);
            tokio::spawn(async move {
                let mut bytes = vec![0_u8; 16 * 1024];
                loop {
                    match reader.read(&mut bytes).await {
                        Ok(0) | Err(_) => {
                            if let Some(keys) = &mut keys {
                                let pending = keys.finish();
                                if !pending.is_empty() {
                                    let _ = input.write(pending).await;
                                }
                            }
                            input.close().await;
                            let _ = ended.send(InputEnd::Closed);
                            return;
                        }
                        Ok(size) => {
                            let (forward, should_detach) = keys.as_mut().map_or_else(
                                || (bytes[..size].to_vec(), false),
                                |keys| keys.consume(&bytes[..size]),
                            );
                            if !forward.is_empty() && input.write(forward).await.is_err() {
                                return;
                            }
                            if should_detach {
                                let _ = ended.send(InputEnd::Detached);
                                return;
                            }
                        }
                    }
                }
            })
        });
        let mut watch_input = input_task.is_some();
        let mut disconnected = false;
        loop {
            let entry = tokio::select! {
                result = &mut end, if watch_input => match result {
                    Ok(InputEnd::Closed) => {
                        disconnected = true;
                        break;
                    }
                    Ok(InputEnd::Detached) => break,
                    Err(_) => {
                        watch_input = false;
                        continue;
                    }
                },
                result = self.session.next() => match result {
                    Ok(Some(entry)) => entry,
                    _ => break,
                },
            };
            let selected = match entry.stream {
                hl_container::Stream::Stdout => self.streams.stdout,
                hl_container::Stream::Stderr => self.streams.stderr,
            };
            let bytes = if self.terminal {
                entry.bytes
            } else {
                ContainerLogs::frame(&entry)
            };
            if selected && writer.write_all(&bytes).await.is_err() {
                disconnected = true;
                break;
            }
        }
        let _ = writer.shutdown().await;
        if let Some(task) = input_task {
            task.abort();
        }
        if disconnected {
            if let Some(disconnect) = self.disconnect {
                tokio::spawn(disconnect.cleanup());
            }
        }
    }
}

struct Disconnect {
    executions: Executions,
    id: ExecId,
}

impl Disconnect {
    async fn cleanup(self) {
        if let Err(error) = self.executions.signal(&self.id, Signal::Kill).await {
            hl_log::hl_warn!(
                hl_log::tag::DAEMON,
                "disconnected exec signal failed id={} error={error}",
                self.id
            );
        }
        for _ in 0..500 {
            match self.executions.remove(&self.id).await {
                Ok(()) | Err(hl_container::Error::NotFound(_)) => return,
                Err(hl_container::Error::InvalidExecState { .. }) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::DAEMON,
                        "disconnected exec cleanup failed id={} error={error}",
                        self.id
                    );
                    return;
                }
            }
        }
        hl_log::hl_error!(
            hl_log::tag::DAEMON,
            "disconnected exec cleanup timed out id={}",
            self.id
        );
    }
}

enum InputEnd {
    Closed,
    Detached,
}

#[derive(Clone)]
pub(super) struct DetachKeys(Vec<u8>);

impl DetachKeys {
    pub(super) fn parse(value: &str) -> ApiResult<Option<Self>> {
        if value.is_empty() {
            return Ok(None);
        }
        let invalid = || {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid detach key sequence {value:?}"),
            )
        };
        let ascii = |token: &str| {
            let bytes = token.as_bytes();
            if bytes.len() == 1 && bytes[0].is_ascii() {
                Ok(bytes[0])
            } else {
                Err(invalid())
            }
        };
        let mut bytes = Vec::new();
        for token in value.split(',') {
            let byte = if let Some(control) = token
                .strip_prefix("ctrl-")
                .or_else(|| token.strip_prefix("CTRL-"))
            {
                let control = ascii(control)?;
                match control {
                    b'@'..=b'_' => control & 0x1f,
                    b'a'..=b'z' => control - b'a' + 1,
                    b'?' => 0x7f,
                    _ => return Err(invalid()),
                }
            } else {
                ascii(token)?
            };
            bytes.push(byte);
        }
        Ok(Some(Self(bytes)))
    }
}

struct DetachInput {
    keys: Vec<u8>,
    pending: Vec<u8>,
}

impl DetachInput {
    fn new(keys: DetachKeys) -> Self {
        Self {
            keys: keys.0,
            pending: Vec::new(),
        }
    }

    fn consume(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let mut forward = Vec::new();
        for byte in bytes {
            self.pending.push(*byte);
            while !self.keys.starts_with(&self.pending) {
                forward.push(self.pending.remove(0));
            }
            if self.pending == self.keys {
                self.pending.clear();
                return (forward, true);
            }
        }
        (forward, false)
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_resize_is_height_then_width_and_bounded() {
        let size = Resize {
            height: 41,
            width: 109,
        }
        .size()
        .unwrap();
        assert_eq!((size.rows(), size.columns()), (41, 109));
        assert!(Resize {
            height: 65_536,
            width: 80,
        }
        .size()
        .is_err());
        assert!(Resize {
            height: 24,
            width: 0,
        }
        .size()
        .is_err());
    }

    #[test]
    fn detach_keys_parse_docker_control_notation() {
        assert_eq!(DetachKeys::parse("").unwrap().map(|keys| keys.0), None);
        assert_eq!(
            DetachKeys::parse("ctrl-p,ctrl-q")
                .unwrap()
                .map(|keys| keys.0),
            Some(vec![16, 17])
        );
        assert_eq!(
            DetachKeys::parse("ctrl-],x").unwrap().map(|keys| keys.0),
            Some(vec![29, b'x'])
        );
        for invalid in ["ctrl-", "ctrl-aa", "ctrl-!", "é"] {
            assert!(DetachKeys::parse(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn detach_input_handles_split_sequences_without_leaking_control_bytes() {
        let mut input = DetachInput::new(DetachKeys::parse("ctrl-p,ctrl-q").unwrap().unwrap());
        assert_eq!(input.consume(b"hello\x10"), (b"hello".to_vec(), false));
        assert_eq!(input.consume(b"x\x10"), (vec![16, b'x'], false));
        assert_eq!(input.consume(b"\x11ignored"), (Vec::new(), true));

        let mut input = DetachInput::new(DetachKeys::parse("ctrl-p,ctrl-q").unwrap().unwrap());
        assert_eq!(input.consume(b"tail\x10"), (b"tail".to_vec(), false));
        assert_eq!(input.finish(), vec![16]);
    }
}
