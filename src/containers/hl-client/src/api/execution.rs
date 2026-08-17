use http::Method;

use crate::model::{ExecAttach, ExecConfig, ExecCreated, ExecInspect, ExecStart, Wait};
use crate::transport::Transport;
use crate::uri::Component;
use crate::{Error, Result};

use super::{Session, Size};

/// Typed operations for processes executed in existing containers.
#[derive(Clone, Copy, Debug)]
pub struct Executions<'a> {
    transport: &'a Transport,
}

impl<'a> Executions<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// Create an execution in an existing container.
    ///
    /// # Errors
    /// Returns transport, validation, container-state, or response-decoding failures.
    pub async fn create(&self, container: &str, config: &ExecConfig) -> Result<ExecCreated> {
        self.transport
            .json(
                Method::POST,
                &format!("/containers/{}/exec", Component::segment(container)),
                Some(config),
            )
            .await
    }

    /// Inspect an execution by ID.
    ///
    /// # Errors
    /// Returns transport, not-found, daemon, or response-decoding failures.
    pub async fn inspect(&self, id: &str) -> Result<ExecInspect> {
        self.transport
            .get_json(&format!("/exec/{}/json", Component::segment(id)))
            .await
    }

    /// Waits without polling for an execution to exit.
    ///
    /// # Errors
    /// Returns transport, lookup, lifecycle, runtime, persistence, or decoding failures.
    pub async fn wait(&self, id: &str) -> Result<Wait> {
        self.transport
            .blocking_json(Method::POST, &format!("/exec/{}/wait", Component::segment(id)))
            .await
    }

    /// Start an execution and attach its configured streams.
    ///
    /// # Errors
    /// Returns a protocol error for a detached request, or transport, upgrade, state, and framing
    /// failures from the daemon.
    pub async fn start(&self, id: &str, config: &ExecStart) -> Result<Session> {
        if config.detach {
            return Err(Error::Protocol("attached exec start cannot set Detach=true".into()));
        }
        let stream = self
            .transport
            .upgrade_json(Method::POST, &format!("/exec/{}/start", Component::segment(id)), config)
            .await?;
        if config.tty {
            Ok(Session::terminal(stream, self.transport.response_limit()))
        } else {
            Ok(Session::pipes(stream, self.transport.response_limit()))
        }
    }

    /// Start an execution without attaching its streams.
    ///
    /// # Errors
    /// Returns a protocol error for an attached request, or transport, state, and daemon failures.
    pub async fn start_detached(&self, id: &str, config: &ExecStart) -> Result<()> {
        if !config.detach {
            return Err(Error::Protocol("detached exec start requires Detach=true".into()));
        }
        self.transport
            .empty_json(Method::POST, &format!("/exec/{}/start", Component::segment(id)), config)
            .await
    }

    /// Attach to an already-running execution without starting it again.
    ///
    /// # Errors
    /// Returns transport, upgrade, state, and framing failures from the daemon.
    pub async fn attach(&self, id: &str, config: &ExecAttach) -> Result<Session> {
        let stream = self
            .transport
            .upgrade_json(
                Method::POST,
                &format!("/exec/{}/attach", Component::segment(id)),
                config,
            )
            .await?;
        if config.tty {
            Ok(Session::terminal(stream, self.transport.response_limit()))
        } else {
            Ok(Session::pipes(stream, self.transport.response_limit()))
        }
    }

    /// Resize a running execution terminal.
    ///
    /// # Errors
    /// Returns transport, lookup, lifecycle, or missing-terminal failures.
    pub async fn resize(&self, id: &str, size: Size) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!("/exec/{}/resize?{}", Component::segment(id), size.query()),
            )
            .await
    }

    /// Deliver a signal to a running execution without signaling its owning container.
    ///
    /// # Errors
    /// Returns transport, signal-validation, lookup, lifecycle, or daemon failures.
    pub async fn signal(&self, id: &str, signal: &str) -> Result<()> {
        self.transport
            .empty(
                Method::POST,
                &format!(
                    "/exec/{}/kill?signal={}",
                    Component::segment(id),
                    Component::opaque(signal)
                ),
            )
            .await
    }

    /// Remove a stopped execution record and its captured output.
    ///
    /// # Errors
    /// Returns transport, lookup, active-execution, cleanup, or daemon failures.
    pub async fn remove(&self, id: &str) -> Result<()> {
        self.transport
            .empty(Method::DELETE, &format!("/exec/{}", Component::segment(id)))
            .await
    }
}
