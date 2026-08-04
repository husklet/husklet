use http::Method;

use crate::model::{
    Change, Container, ContainerCreation, ContainerLogs, ContainerPrune, CreateContainer,
    InspectContainer, List, LogOptions, Stats, Top, Update, UpdateResult, Wait,
};
use crate::transport::Transport;
use crate::uri::Component;
use crate::Result;

use super::{LogStream, Session, Size};

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
    pub async fn create(
        &self,
        request: &CreateContainer,
        name: Option<&str>,
    ) -> Result<ContainerCreation> {
        let path = name.map_or_else(
            || "/containers/create".to_owned(),
            |value| format!("/containers/create?name={}", Component::opaque(value)),
        );
        self.transport
            .json(Method::POST, &path, Some(request))
            .await
    }

    /// List containers, optionally including stopped containers.
    ///
    /// # Errors
    /// Returns transport, protocol, decoding, or daemon failures.
    pub async fn list(&self, selection: impl Into<List>) -> Result<Vec<Container>> {
        let selection = selection.into();
        let filters = serde_json::to_string(selection.values())
            .map_err(|error| crate::Error::Protocol(error.to_string()))?;
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
        let filters = serde_json::to_string(filters)
            .map_err(|error| crate::Error::Protocol(error.to_string()))?;
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
            .get_json(&format!(
                "/containers/{}/json?size={size}",
                Component::opaque(id)
            ))
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
                &format!(
                    "/containers/{}/logs?{}",
                    Component::opaque(id),
                    query.join("&")
                ),
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
    pub async fn attach(
        &self,
        id: &str,
        stdin: bool,
        stdout: bool,
        stderr: bool,
    ) -> Result<Session> {
        let path = format!(
            "/containers/{}/attach?stream=true&stdin={stdin}&stdout={stdout}&stderr={stderr}",
            Component::opaque(id)
        );
        let stream = self.transport.upgrade(Method::POST, &path).await?;
        Ok(Session::pipes(stream, self.transport.response_limit()))
    }

    /// Attach to a controlling terminal using its raw merged byte stream.
    ///
    /// # Errors
    /// Returns transport, upgrade, or container-state failures.
    pub async fn attach_terminal(&self, id: &str, stdin: bool) -> Result<Session> {
        let path = format!(
            "/containers/{}/attach?stream=true&stdin={stdin}&stdout=true&stderr=true",
            Component::opaque(id)
        );
        let stream = self.transport.upgrade(Method::POST, &path).await?;
        Ok(Session::terminal(stream, self.transport.response_limit()))
    }

    /// Read metadata for a path in the container filesystem.
    ///
    /// # Errors
    /// Start the container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn start(&self, id: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/start", Component::opaque(id)),
            )
            .await
    }

    /// Resize a running container terminal.
    ///
    /// # Errors
    /// Returns transport, lookup, lifecycle, or missing-terminal failures.
    pub async fn resize(&self, id: &str, size: Size) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/resize?{}",
                    Component::opaque(id),
                    size.query()
                ),
            )
            .await
    }

    /// Gracefully stop a running container, then force it after the optional timeout.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn stop(&self, id: &str, timeout_seconds: Option<u64>) -> Result<()> {
        let query = timeout_seconds.map_or_else(String::new, |seconds| format!("?t={seconds}"));
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/stop{query}", Component::opaque(id)),
            )
            .await
    }

    /// Stop and start a running container.
    ///
    /// # Errors
    /// Returns transport, state, runtime, or daemon failures.
    pub async fn restart(&self, id: &str, timeout_seconds: Option<u64>) -> Result<()> {
        let query = timeout_seconds.map_or_else(String::new, |seconds| format!("?t={seconds}"));
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/restart{query}", Component::opaque(id)),
            )
            .await
    }

    /// Deliver a Linux signal name or number to a running container.
    ///
    /// # Errors
    /// Returns transport, signal-validation, state, runtime, or daemon failures.
    pub async fn kill(&self, id: &str, signal: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/kill?signal={}",
                    Component::opaque(id),
                    Component::opaque(signal)
                ),
            )
            .await
    }

    /// Atomically assign a new unique container name.
    ///
    /// # Errors
    /// Returns transport, validation, conflict, persistence, or daemon failures.
    pub async fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/rename?name={}",
                    Component::opaque(id),
                    Component::opaque(name)
                ),
            )
            .await
    }

    /// Suspend the container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, process-control, or daemon failures.
    pub async fn pause(&self, id: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/pause", Component::opaque(id)),
            )
            .await
    }

    /// Resume a paused container's initial process.
    ///
    /// # Errors
    /// Returns transport, state, process-control, or daemon failures.
    pub async fn unpause(&self, id: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!("/containers/{}/unpause", Component::opaque(id)),
            )
            .await
    }

    /// Persist the complete container process tree and stop the live engine.
    ///
    /// A subsequent [`Self::start`] restores the container from the published checkpoint.
    ///
    /// # Errors
    /// Returns transport, state, timeout, resource-compatibility, or daemon failures.
    pub async fn checkpoint(&self, id: &str, timeout: std::time::Duration) -> Result<()> {
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/containers/{}/checkpoint?timeout_ms={timeout_ms}",
                    Component::opaque(id)
                ),
            )
            .await
    }
    /// Wait for the container's initial process to stop.
    ///
    /// # Errors
    /// Returns transport, state, runtime, decoding, or daemon failures.
    pub async fn wait(&self, id: &str) -> Result<Wait> {
        self.wait_for(id, WaitCondition::NotRunning).await
    }

    /// Wait for a specific Docker lifecycle condition.
    ///
    /// # Errors
    /// Returns transport, state, runtime, decoding, or daemon failures.
    pub async fn wait_for(&self, id: &str, condition: WaitCondition) -> Result<Wait> {
        self.transport
            .json::<(), Wait>(
                Method::POST,
                &format!(
                    "/containers/{}/wait?condition={}",
                    Component::opaque(id),
                    condition.as_str()
                ),
                None,
            )
            .await
    }
    /// Remove a container and optionally force-stop it first.
    ///
    /// # Errors
    /// Returns transport, state, runtime, persistence, or daemon failures.
    pub async fn remove(&self, id: &str, force: bool, volumes: bool) -> Result<()> {
        self.transport
            .empty(
                Method::DELETE,
                &format!(
                    "/containers/{}?force={force}&v={volumes}",
                    Component::opaque(id)
                ),
            )
            .await
    }
}

mod archive;
pub use archive::Archive;
mod stats;
pub use stats::StatsStream;
