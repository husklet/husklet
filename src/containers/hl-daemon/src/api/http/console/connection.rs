use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use hl_container::{Input, Session, Streams};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

use crate::api::ContainerLogs;

use super::detach::DetachInput;
use super::{DetachKeys, Disconnect};
use crate::api::http::error::ApiResult;

pub(in crate::api::http) struct Connection {
    upgrade: hyper::upgrade::OnUpgrade,
    session: Session,
    streams: Streams,
    terminal: bool,
    content_type: &'static str,
    logs: bool,
    stream: bool,
    detach: Option<DetachKeys>,
    disconnect: Option<Disconnect>,
}

impl Connection {
    pub(in crate::api::http) fn new(
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
            content_type: "application/vnd.docker.raw-stream",
            logs: false,
            stream: true,
            detach: None,
            disconnect: None,
        }
    }

    pub(in crate::api::http) fn content_type(mut self, content_type: &'static str) -> Self {
        self.content_type = content_type;
        self
    }

    pub(in crate::api::http) fn output(mut self, logs: bool, stream: bool) -> Self {
        self.logs = logs;
        self.stream = stream;
        self
    }

    pub(in crate::api::http) fn kill_on_disconnect(
        mut self,
        executions: hl_container::Executions,
        id: hl_container::ExecId,
    ) -> Self {
        self.disconnect = Some(Disconnect { executions, id });
        self
    }

    pub(in crate::api::http) fn detach_keys(mut self, value: &str) -> ApiResult<Self> {
        self.detach = DetachKeys::parse(value)?;
        Ok(self)
    }

    pub(in crate::api::http) fn spawn(self) -> Response {
        let response = Self::response(self.content_type);
        tokio::spawn(self.run());
        response
    }

    async fn run(mut self) {
        let streams = self.streams;
        let terminal = self.terminal;
        let Ok(Ok(upgraded)) = tokio::time::timeout(std::time::Duration::from_secs(5), self.upgrade).await else {
            Self::cleanup_disconnect(&mut self.disconnect).await;
            return;
        };
        let stream = hyper_util::rt::TokioIo::new(upgraded);
        let (reader, mut writer) = tokio::io::split(stream);
        let (ended, mut end) = tokio::sync::oneshot::channel();
        let input_task = (self.stream && (streams.stdin || self.disconnect.is_some())).then(|| {
            let forwarder = InputForwarder {
                input: self.session.input(),
                keys: self.detach.take().map(DetachInput::new),
                ended,
            };
            tokio::spawn(forwarder.run(reader))
        });
        if self.logs
            && Self::write_history(&mut self.session, &mut writer, streams, terminal)
                .await
                .is_err()
        {
            Self::finish(&mut writer, input_task).await;
            Self::cleanup_disconnect(&mut self.disconnect).await;
            return;
        }
        if !self.stream || (!streams.stdin && !streams.stdout && !streams.stderr) {
            Self::finish(&mut writer, input_task).await;
            return;
        }
        let mut watch_input = input_task.is_some();
        let mut disconnected = false;
        loop {
            let entry = tokio::select! {
                result = &mut end, if watch_input => match result {
                    // Closing stdin is a half-close, not a disconnect: the process keeps
                    // producing output that this session still owes the client. Stop
                    // watching input and keep draining; a client that really went away
                    // surfaces as a write failure below.
                    Ok(InputEnd::Closed) => {
                        // An owning exec attachment uses socket lifetime as
                        // process lifetime. EOF is therefore a full owner
                        // disconnect, even when stdin was enabled; otherwise a
                        // quiet process can retain its attachment lease and
                        // kill-on-disconnect authority forever.
                        if self.disconnect.is_some() || !streams.stdin {
                            disconnected = true;
                            break;
                        }
                        watch_input = false;
                        continue;
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
            let sequence = entry.sequence;
            if Self::write_entry(&mut writer, entry, streams, terminal).await.is_err() {
                disconnected = true;
                break;
            }
            self.session.acknowledge(sequence);
        }
        let _ = writer.shutdown().await;
        if let Some(task) = input_task {
            task.abort();
        }
        if disconnected {
            Self::cleanup_disconnect(&mut self.disconnect).await;
        }
    }

    async fn cleanup_disconnect(disconnect: &mut Option<Disconnect>) {
        if let Some(disconnect) = disconnect.take() {
            disconnect.cleanup().await;
        }
    }

    async fn write_history<W>(
        session: &mut Session,
        writer: &mut W,
        streams: Streams,
        terminal: bool,
    ) -> std::io::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let history = session.history().await.map_err(std::io::Error::other)?;
        for entry in history {
            let sequence = entry.sequence;
            Self::write_entry(writer, entry, streams, terminal).await?;
            session.acknowledge(sequence);
        }
        Ok(())
    }

    async fn finish<W>(writer: &mut W, input_task: Option<tokio::task::JoinHandle<()>>)
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let _ = writer.shutdown().await;
        if let Some(task) = input_task {
            task.abort();
        }
    }

    async fn write_entry<W>(
        writer: &mut W,
        entry: hl_container::Entry,
        streams: Streams,
        terminal: bool,
    ) -> std::io::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let selected = match entry.stream {
            hl_container::Stream::Stdout => streams.stdout,
            hl_container::Stream::Stderr => streams.stderr,
        };
        if !selected {
            return Ok(());
        }
        let bytes = if terminal {
            entry.bytes
        } else {
            ContainerLogs::frame(&entry)
        };
        writer.write_all(&bytes).await
    }

    fn response(content_type: &'static str) -> Response {
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "tcp")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::empty())
            .expect("static console response is valid")
    }
}

struct InputForwarder {
    input: Input,
    keys: Option<DetachInput>,
    ended: tokio::sync::oneshot::Sender<InputEnd>,
}

impl InputForwarder {
    async fn run<R>(mut self, mut reader: R)
    where
        R: AsyncRead + Unpin,
    {
        let mut bytes = vec![0_u8; 16 * 1024];
        loop {
            let size = reader.read(&mut bytes).await.unwrap_or_default();
            if size == 0 {
                self.close().await;
                let _ = self.ended.send(InputEnd::Closed);
                return;
            }
            let (forward, should_detach) = self.filter(&bytes[..size]);
            if self.forward(forward).await.is_err() {
                return;
            }
            if should_detach {
                let _ = self.ended.send(InputEnd::Detached);
                return;
            }
        }
    }

    async fn close(&mut self) {
        let pending = self.keys.as_mut().map_or_else(Vec::new, DetachInput::finish);
        if self.forward(pending).await.is_err() {
            self.input.close().await;
            return;
        }
        self.input.close().await;
    }

    fn filter(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        self.keys
            .as_mut()
            .map_or_else(|| (bytes.to_vec(), false), |keys| keys.consume(bytes))
    }

    async fn forward(&self, bytes: Vec<u8>) -> Result<(), ()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.input.write(bytes).await.map_err(|_| ())
    }
}

enum InputEnd {
    Closed,
    Detached,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_headers() {
        for content_type in [
            "application/vnd.docker.raw-stream",
            "application/vnd.docker.multiplexed-stream",
        ] {
            let response = Connection::response(content_type);
            assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
            assert_eq!(response.headers().get(header::CONNECTION).unwrap(), "Upgrade");
            assert_eq!(response.headers().get(header::UPGRADE).unwrap(), "tcp");
            assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), content_type);
        }
    }
}
