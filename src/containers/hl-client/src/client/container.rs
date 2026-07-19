//! Container endpoints.

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    StartContainerOptions,
};
use futures_util::StreamExt;

use crate::{Client, Container, CreateContainer, Result};

impl Client {
    /// `GET /containers/json?all=1` (we always include stopped containers).
    pub async fn list_containers(&self) -> Result<Vec<Container>> {
        let opts = ListContainersOptionsBuilder::new().all(true).build();
        let cs = self.docker()?.list_containers(Some(opts)).await?;
        Ok(cs.into_iter().map(Container::from).collect())
    }

    /// `POST /containers/create` — returns the new container id.
    pub async fn create_container(&self, spec: &CreateContainer) -> Result<String> {
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            ..Default::default()
        };
        Ok(self.docker()?.create_container(None, body).await?.id)
    }

    /// `POST /containers/{id}/start`.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.docker()?
            .start_container(id, None::<StartContainerOptions>)
            .await
    }

    /// `POST /containers/{id}/stop`.
    pub async fn stop_container(&self, id: &str) -> Result<()> {
        self.docker()?
            .stop_container(id, None::<bollard::query_parameters::StopContainerOptions>)
            .await
    }

    /// `POST /containers/{id}/restart`.
    pub async fn restart_container(&self, id: &str) -> Result<()> {
        self.docker()?
            .restart_container(
                id,
                None::<bollard::query_parameters::RestartContainerOptions>,
            )
            .await
    }

    /// `POST /containers/{id}/pause`.
    pub async fn pause_container(&self, id: &str) -> Result<()> {
        self.docker()?.pause_container(id).await
    }

    /// `POST /containers/{id}/unpause`.
    pub async fn unpause_container(&self, id: &str) -> Result<()> {
        self.docker()?.unpause_container(id).await
    }

    /// `DELETE /containers/{id}` (force, so running containers are removed too).
    pub async fn remove_container(&self, id: &str) -> Result<()> {
        let opts = RemoveContainerOptionsBuilder::new().force(true).build();
        self.docker()?.remove_container(id, Some(opts)).await
    }

    /// `GET /containers/{id}/logs` — concatenated stdout+stderr bytes (bollard demuxes the Docker
    /// multiplexed frames into `LogOutput` chunks for us).
    pub async fn container_logs(&self, id: &str) -> Result<Vec<u8>> {
        let opts = LogsOptionsBuilder::new().stdout(true).stderr(true).build();
        let mut stream = self.docker()?.logs(id, Some(opts));
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let output = item?;
            out.extend_from_slice(&output.into_bytes());
        }
        Ok(out)
    }
}
