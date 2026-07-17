//! Image endpoints.

use bollard::query_parameters::{ListImagesOptionsBuilder, RemoveImageOptions};

use crate::{Client, Image, Result};

impl Client {
    /// `GET /images/json`.
    pub async fn list_images(&self) -> Result<Vec<Image>> {
        let opts = ListImagesOptionsBuilder::new().build();
        let imgs = self.docker()?.list_images(Some(opts)).await?;
        Ok(imgs.into_iter().map(Image::from).collect())
    }

    /// `DELETE /images/{name}`.
    pub async fn remove_image(&self, name: &str) -> Result<()> {
        self.docker()?
            .remove_image(name, None::<RemoveImageOptions>, None)
            .await
            .map(|_| ())
    }
}
