//! Launch capabilities composed for one workspace.

use crate::config::WorkspaceConfig;
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

/// Product-owned composition of the independently implemented workspace devices.
pub struct Workspace {
    graphics: Option<crate::runtime::gpu::Graphics>,
    docker: Option<PathBuf>,
}

impl Workspace {
    pub fn new(config: &WorkspaceConfig) -> io::Result<Self> {
        let graphics = crate::runtime::gpu::Graphics::for_workspace(config)?;
        let docker = config
            .docker_sock
            .then(|| crate::runtime::resources::Daemon::new(config).ensure())
            .transpose()?;
        Ok(Self { graphics, docker })
    }
}

impl hl_container::Device for Workspace {
    fn name(&self) -> &str {
        "workspace"
    }

    fn request(
        &self,
        context: hl_container::DeviceContext<'_>,
    ) -> hl_container::Result<hl_container::DeviceRequest> {
        let mut request = self.graphics.as_ref().map_or_else(
            || Ok(Default::default()),
            |graphics| graphics.request(context),
        )?;
        let Some(host) = &self.docker else {
            return Ok(request);
        };

        request.environment.insert(
            "DOCKER_HOST".to_owned(),
            "unix:///run/docker.sock".to_owned(),
        );
        let socket = hl_container::device::extension::NamespaceEntry::Socket(
            hl_container::device::extension::SocketEntry {
                path: "/run/docker.sock".into(),
                host: host.clone(),
            },
        );
        if let Some(namespace) = request
            .extensions
            .iter_mut()
            .find(|extension| extension.provider.as_str() == "engine.namespace")
        {
            namespace
                .required_features
                .insert(hl_container::device::extension::Feature::new(
                    "unix-sockets",
                )?);
            namespace.namespace.push(socket);
        } else {
            request
                .extensions
                .push(hl_container::device::extension::ExtensionSpec {
                    provider: hl_container::device::extension::ProviderId::new("engine.namespace")?,
                    version: hl_container::device::Version::new(1, 0),
                    required: true,
                    required_features: BTreeSet::from([
                        hl_container::device::extension::Feature::new("unix-sockets")?,
                    ]),
                    optional_features: BTreeSet::new(),
                    config: hl_container::device::extension::ExtensionConfig::empty(
                        "engine.namespace/v1",
                    ),
                    namespace: vec![socket],
                    rules: Vec::new(),
                    services: Vec::new(),
                    memory: Vec::new(),
                    environment: Vec::new(),
                });
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_uses_typed_socket_projection() {
        let workspace = Workspace {
            graphics: None,
            docker: Some("/host/docker.sock".into()),
        };
        let process = hl_container::Process::new("/bin/true");
        let root = std::path::PathBuf::from("/");
        let request = hl_container::Device::request(
            &workspace,
            hl_container::DeviceContext {
                guest: hl_container::Guest::Aarch64,
                process: &process,
                filesystem: hl_container::device::FilesystemView::new(&root, None),
            },
        )
        .unwrap();

        assert!(request.mounts.is_empty());
        assert_eq!(
            request.environment["DOCKER_HOST"],
            "unix:///run/docker.sock"
        );
        assert!(matches!(
            &request.extensions[0].namespace[0],
            hl_container::device::extension::NamespaceEntry::Socket(socket)
                if socket.path == std::path::Path::new("/run/docker.sock")
                    && socket.host == std::path::Path::new("/host/docker.sock")
        ));
    }
}
