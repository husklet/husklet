use http::Method;

use crate::Result;
use crate::model::{
    Change, Container, ContainerCreation, ContainerLogs, ContainerPrune, CreateContainer, InspectContainer, List,
    LogOptions, Stats, Top, Update, UpdateResult,
};
use crate::transport::Transport;
use crate::uri::Component;

use super::{LogStream, Session};

/// Typed container operations.
#[derive(Clone, Copy, Debug)]
pub struct Containers<'a> {
    transport: &'a Transport,
}

/// Docker container wait condition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitCondition {
    NotRunning,
    NextExit,
    Removed,
}

/// Docker attach history, live-stream, and standard-stream selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AttachOptions {
    /// Replay output written before the attachment was opened.
    pub logs: bool,
    /// Continue with live streams after the attachment boundary.
    pub stream: bool,
    /// Forward client input to the container process.
    pub stdin: bool,
    /// Return process standard output.
    pub stdout: bool,
    /// Return process standard error.
    pub stderr: bool,
    /// Override Docker's detach key sequence.
    pub detach_keys: Option<String>,
}

impl WaitCondition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotRunning => "not-running",
            Self::NextExit => "next-exit",
            Self::Removed => "removed",
        }
    }
}

impl<'a> Containers<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// Create a container from a locally available image.
    ///
    /// # Errors
    /// Returns transport, protocol, decoding, validation, image, or daemon lifecycle failures.
    pub async fn create(&self, request: &CreateContainer, name: Option<&str>) -> Result<ContainerCreation> {
        let path = name.map_or_else(
            || "/containers/create".to_owned(),
            |value| format!("/containers/create?name={}", Component::opaque(value)),
        );
        self.transport.json(Method::POST, &path, Some(request)).await
    }

    /// List containers, optionally including stopped containers.
    ///
    /// # Errors
    /// Returns transport, protocol, decoding, or daemon failures.
    pub async fn list(&self, selection: impl Into<List>) -> Result<Vec<Container>> {
        let selection = selection.into();
        let filters =
            serde_json::to_string(selection.values()).map_err(|error| crate::Error::Protocol(error.to_string()))?;
        self.transport
            .get_json(&format!(
                "/containers/json?all={}&filters={}",
                selection.includes_inactive(),
                Component::opaque(&filters)
            ))
            .await
    }

    /// Remove every stopped or never-started container.
    ///
    /// # Errors
    /// Returns transport, persistence, owned-resource cleanup, or decoding failures.
    pub async fn prune(&self) -> Result<ContainerPrune> {
        self.prune_with(&std::collections::BTreeMap::new()).await
    }

    /// Remove inactive containers selected by Docker `until`, `label`, and `label!` filters.
    ///
    /// # Errors
    /// Returns filter serialization, validation, persistence, cleanup, or decoding failures.
    pub async fn prune_with(
        &self,
        filters: &std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<ContainerPrune> {
        let filters = serde_json::to_string(filters).map_err(|error| crate::Error::Protocol(error.to_string()))?;
        self.transport
            .json::<(), ContainerPrune>(
                Method::POST,
                &format!("/containers/prune?filters={}", Component::opaque(&filters)),
                None,
            )
            .await
    }
    /// Inspect a container by ID, prefix, or name.
    ///
    /// # Errors
    /// Returns transport, protocol, decoding, not-found, or daemon failures.
    pub async fn inspect(&self, id: &str) -> Result<InspectContainer> {
        self.inspect_with_size(id, false).await
    }

    /// Inspect a container and optionally include rootfs byte accounting.
    ///
    /// # Errors
    /// Returns transport, protocol, decoding, not-found, storage, or daemon failures.
    pub async fn inspect_with_size(&self, id: &str, size: bool) -> Result<InspectContainer> {
        self.transport
            .get_json(&format!("/containers/{}/json?size={size}", Component::opaque(id)))
            .await
    }

    /// Lists paths changed from the container's immutable image baseline.
    ///
    /// # Errors
    /// Returns transport, decoding, lookup, active-container, or missing-baseline failures.
    pub async fn changes(&self, id: &str) -> Result<Vec<Change>> {
        self.transport
            .get_json(&format!("/containers/{}/changes", Component::opaque(id)))
            .await
    }

    /// Updates runtime-effective mutable container settings.
    ///
    /// Resource changes require an inactive container; restart-policy changes are durable for
    /// subsequent lifecycle decisions.
    ///
    /// # Errors
    /// Returns transport, validation, lookup, persistence, or active-resource-change failures.
    pub async fn update(&self, id: &str, update: &Update) -> Result<UpdateResult> {
        self.transport
            .json(
                Method::POST,
                &format!("/containers/{}/update", Component::opaque(id)),
                Some(update),
            )
            .await
    }

    /// List the container's live processes in Docker's tabular shape.
    ///
    /// # Errors
    /// Returns transport, decoding, not-found, or inactive-container failures.
    pub async fn top(&self, id: &str) -> Result<Top> {
        self.top_with(id, None).await
    }

    /// List live processes using an explicit Docker `ps_args` column selection.
    ///
    /// # Errors
    /// Returns transport, decoding, not-found, inactive-container, or unsupported-column failures.
    pub async fn top_with(&self, id: &str, ps_args: Option<&str>) -> Result<Top> {
        let query = ps_args
            .map(|value| format!("?ps_args={}", Component::opaque(value)))
            .unwrap_or_default();
        self.transport
            .get_json(&format!("/containers/{}/top{query}", Component::opaque(id)))
            .await
    }

    /// Read one resource-usage sample for a container.
    ///
    /// # Errors
    /// Returns transport, decoding, or not-found failures.
    pub async fn stats(&self, id: &str) -> Result<Stats> {
        self.transport
            .get_json(&format!(
                "/containers/{}/stats?stream=false&one-shot=true",
                Component::opaque(id)
            ))
            .await
    }

    /// Subscribe to periodic resource samples until the container exits or the stream is dropped.
    ///
    /// # Errors
    /// Returns transport, HTTP, or not-found failures while opening the stream.
    pub async fn stats_stream(&self, id: &str) -> Result<StatsStream> {
        let stream = self
            .transport
            .stream(
                Method::GET,
                &format!("/containers/{}/stats?stream=true", Component::opaque(id)),
            )
            .await?;
        Ok(StatsStream::new(stream))
    }

    /// Read captured initial-process output.
    ///
    /// # Errors
    /// Returns transport, framing, not-found, or daemon failures.
    pub async fn logs(&self, id: &str, stdout: bool, stderr: bool) -> Result<ContainerLogs> {
        let bytes = self
            .transport
            .get(&format!(
                "/containers/{}/logs?stdout={stdout}&stderr={stderr}",
                Component::opaque(id)
            ))
            .await?;
        ContainerLogs::decode(&bytes).map_err(|error| crate::Error::Protocol(error.to_string()))
    }

    /// Replay and optionally follow ordered container output as Docker stream frames.
    ///
    /// # Errors
    /// Returns transport, framing, selection, not-found, or daemon failures.
    pub async fn logs_stream(&self, id: &str, options: &LogOptions) -> Result<LogStream> {
        let stream = self.open_logs(id, options).await?;
        Ok(LogStream::new(stream, self.transport.response_limit()))
    }

    /// Replay or follow logs for a TTY container as raw merged terminal output.
    ///
    /// # Errors
    /// Returns transport, selection, not-found, or daemon failures.
    pub async fn logs_terminal_stream(&self, id: &str, options: &LogOptions) -> Result<LogStream> {
        let stream = self.open_logs(id, options).await?;
        Ok(LogStream::terminal(stream, self.transport.response_limit()))
    }

    async fn open_logs(&self, id: &str, options: &LogOptions) -> Result<crate::Stream> {
        let mut query = vec![
            format!("follow={}", options.follow),
            format!("stdout={}", options.streams.stdout),
            format!("stderr={}", options.streams.stderr),
            format!("timestamps={}", options.timestamps),
        ];
        for (name, milliseconds) in [("since", options.since_ms), ("until", options.until_ms)] {
            let Some(milliseconds) = milliseconds else {
                continue;
            };
            let seconds = milliseconds / 1_000;
            let fraction = milliseconds % 1_000;
            let value = if fraction == 0 {
                seconds.to_string()
            } else {
                format!("{seconds}.{fraction:03}")
            };
            query.push(format!("{name}={value}"));
        }
        if let Some(tail) = options.tail {
            query.push(format!("tail={tail}"));
        }
        let stream = self
            .transport
            .stream(
                Method::GET,
                &format!("/containers/{}/logs?{}", Component::opaque(id), query.join("&")),
            )
            .await?;
        Ok(stream)
    }

    /// Attach to the initial process over Docker's bidirectional stream protocol.
    ///
    /// The returned session can write standard input and reads stdout/stderr in daemon order.
    ///
    /// # Errors
    /// Returns transport, upgrade, state, or multiplex framing failures.
    pub async fn attach(&self, id: &str, stdin: bool, stdout: bool, stderr: bool) -> Result<Session> {
        self.attach_with(
            id,
            &AttachOptions {
                stream: true,
                stdin,
                stdout,
                stderr,
                ..AttachOptions::default()
            },
        )
        .await
    }

    /// Attach to selected initial-process history and/or live streams.
    ///
    /// # Errors
    /// Returns transport, upgrade, state, or multiplex framing failures.
    pub async fn attach_with(&self, id: &str, options: &AttachOptions) -> Result<Session> {
        let stream = self
            .transport
            .upgrade(Method::POST, &self.attach_path(id, options))
            .await?;
        Ok(Session::pipes(stream, self.transport.response_limit()))
    }

    /// Attach to a controlling terminal using its raw merged byte stream.
    ///
    /// # Errors
    /// Returns transport, upgrade, or container-state failures.
    pub async fn attach_terminal(&self, id: &str, stdin: bool) -> Result<Session> {
        self.attach_terminal_with(
            id,
            &AttachOptions {
                stream: true,
                stdin,
                stdout: true,
                stderr: true,
                ..AttachOptions::default()
            },
        )
        .await
    }

    /// Attach to selected controlling-terminal history and/or live output.
    ///
    /// # Errors
    /// Returns transport, upgrade, or container-state failures.
    pub async fn attach_terminal_with(&self, id: &str, options: &AttachOptions) -> Result<Session> {
        let stream = self
            .transport
            .upgrade(Method::POST, &self.attach_path(id, options))
            .await?;
        Ok(Session::terminal(stream, self.transport.response_limit()))
    }

    fn attach_path(&self, id: &str, options: &AttachOptions) -> String {
        let mut path = format!(
            "/containers/{}/attach?logs={}&stream={}&stdin={}&stdout={}&stderr={}",
            Component::opaque(id),
            options.logs,
            options.stream,
            options.stdin,
            options.stdout,
            options.stderr,
        );
        if let Some(keys) = &options.detach_keys {
            path.push_str("&detachKeys=");
            path.push_str(&Component::opaque(keys).to_string());
        }
        path
    }
}

mod archive;
mod lifecycle;
pub use archive::Archive;
mod stats;
pub use stats::StatsStream;
