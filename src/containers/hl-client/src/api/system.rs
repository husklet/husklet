use crate::model::{Authentication, Credentials, DiskUsage, Plugin, SystemInfo, SystemPrune};
use crate::transport::Transport;
use crate::Result;
use http::Method;

/// Typed daemon-wide status and storage operations.
#[derive(Clone, Copy, Debug)]
pub struct System<'a> {
    transport: &'a Transport,
}

impl<'a> System<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// Read daemon capabilities and live object counts.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn info(&self) -> Result<SystemInfo> {
        self.transport.get_json("/info").await
    }

    /// List installed daemon plugins.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn plugins(&self) -> Result<Vec<Plugin>> {
        self.transport.get_json("/plugins").await
    }

    /// Validate registry credentials against the daemon's configured authentication policy.
    ///
    /// # Errors
    /// Returns transport, authentication, or response-decoding failures.
    pub async fn authenticate(&self, credentials: &Credentials) -> Result<Authentication> {
        self.transport
            .json(Method::POST, "/auth", Some(credentials))
            .await
    }

    /// Read durable image/container disk-usage accounting.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn disk_usage(&self) -> Result<DiskUsage> {
        self.transport.get_json("/system/df").await
    }

    /// Remove every unused container, network, image object, and optionally volume.
    ///
    /// # Errors
    /// Returns transport, Docker API, persistence, or response-decoding failures.
    pub async fn prune(&self, volumes: bool) -> Result<SystemPrune> {
        self.prune_with(volumes, &std::collections::BTreeMap::new())
            .await
    }

    /// Remove unused resources using Docker `until`, `label`, and `label!` filters.
    ///
    /// # Errors
    /// Returns filter serialization, Docker API, persistence, or decoding failures.
    pub async fn prune_with(
        &self,
        volumes: bool,
        filters: &std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<SystemPrune> {
        let filters = serde_json::to_string(filters)
            .map_err(|error| crate::Error::Protocol(error.to_string()))?;
        self.transport
            .json::<(), SystemPrune>(
                Method::POST,
                &format!(
                    "/system/prune?volumes={volumes}&filters={}",
                    percent_encoding::utf8_percent_encode(
                        &filters,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ),
                None,
            )
            .await
    }
}
