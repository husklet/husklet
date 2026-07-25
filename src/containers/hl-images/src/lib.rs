//! Runtime-neutral OCI content, image, layer, snapshot, and rootfs primitives.

pub mod build;
pub mod content;
mod digest;
mod error;
mod image;
mod images;
mod lease;
mod platform;
mod reference;
mod transfer;

pub mod format;
pub mod layer;
pub mod remote;
pub mod rootfs;
pub mod snapshot;
pub mod storage;

pub use digest::Digest;
pub use error::{Error, Result};
pub use image::{FsImageStore, Graph, Image, ImageStore};
pub use images::{
    GcReport, History, ImageUsage, Images, Metadata, RuntimeConfig, RuntimeOverrides, UnpackedImage,
};
pub use lease::{Lease, LeaseStore, Leases};
pub use oci_spec::image::Descriptor;
pub use platform::{Platform, PlatformError};
pub use reference::Reference;
pub use transfer::{copy_graph, CopyReport, DescriptorGraph, Successors, Target};

pub(crate) trait DescriptorKind {
    fn is_index(&self) -> bool;
    fn is_manifest(&self) -> bool;
    fn is_document(&self) -> bool;
}

impl DescriptorKind for Descriptor {
    fn is_index(&self) -> bool {
        matches!(
            self.media_type().to_string().as_str(),
            "application/vnd.oci.image.index.v1+json"
                | "application/vnd.docker.distribution.manifest.list.v2+json"
        )
    }

    fn is_manifest(&self) -> bool {
        matches!(
            self.media_type().to_string().as_str(),
            "application/vnd.oci.image.manifest.v1+json"
                | "application/vnd.docker.distribution.manifest.v2+json"
        )
    }

    fn is_document(&self) -> bool {
        let media = self.media_type().to_string();
        media.contains("manifest") || media.contains("image.index")
    }
}
