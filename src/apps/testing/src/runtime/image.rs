use super::{Error, workspace};
use hl_images::{
    Image, Images, Platform, Reference, RuntimeConfig,
    remote::{Auth, Registry},
    rootfs::{Reference as RootReference, View},
};
use std::path::Path;

pub struct TestImage {
    images: Images,
    reference: RootReference,
    view: View,
    runtime: RuntimeConfig,
}

impl TestImage {
    pub async fn materialize(name: &str, platform: &Platform) -> Result<Self, Error> {
        let cache = workspace()?
            .join("target/testing/images")
            .join(platform.architecture.as_str());
        let images = Images::open(cache)?;
        let reference: Reference = name.parse()?;
        let image = match images.resolve(&reference)? {
            Some(image) if images.details(&image, platform).is_ok() => image,
            _ => {
                images
                    .pull(&Registry::new(Auth::Anonymous), reference, platform)
                    .await?
            }
        };
        Self::from_image(images, &image, platform)
    }

    fn from_image(images: Images, image: &Image, platform: &Platform) -> Result<Self, Error> {
        let unpacked = images.unpack(image, platform)?;
        let runtime = unpacked.runtime().clone();
        let reference = images.rootfs(&unpacked)?;
        let view = images.roots().open(&reference)?;
        Ok(Self {
            images,
            reference,
            view,
            runtime,
        })
    }

    pub fn path(&self) -> &Path {
        self.view.path()
    }

    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    pub fn release(self) -> Result<(), Error> {
        self.images.roots().release(&self.reference)?;
        Ok(())
    }
}
