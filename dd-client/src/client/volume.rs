//! Volume endpoints.

use bollard::models::VolumeCreateRequest;
use bollard::query_parameters::{ListVolumesOptions, RemoveVolumeOptions};

use crate::{Client, Result, Volume};

impl Client {
    /// `GET /volumes`.
    pub async fn list_volumes(&self) -> Result<Vec<Volume>> {
        let resp = self
            .docker()?
            .list_volumes(None::<ListVolumesOptions>)
            .await?;
        Ok(resp
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(Volume::from)
            .collect())
    }

    /// `DELETE /volumes/{name}`.
    pub async fn remove_volume(&self, name: &str) -> Result<()> {
        self.docker()?
            .remove_volume(name, None::<RemoveVolumeOptions>)
            .await
    }

    /// `POST /volumes/create`.
    pub async fn create_volume(&self, name: &str) -> Result<()> {
        let body = VolumeCreateRequest {
            name: Some(name.to_string()),
            ..Default::default()
        };
        self.docker()?.create_volume(body).await.map(|_| ())
    }
}
