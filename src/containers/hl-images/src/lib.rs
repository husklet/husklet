//! `hl-images` — image handling for hl, kept separate from the container runtime (`hl-jit`) and the
//! Docker polyfill (`hl-daemon`). Today it owns the OCI **registry client** ([`registry`]): pulling and
//! pushing manifests + layers, registry auth, and image references. Image/rootfs *building* (Dockerfile
//! execution, layer extraction) will consolidate here too as it is decoupled from the daemon runtime.
#![warn(missing_docs)]

mod error;
pub use error::Error;

pub mod registry;
pub use registry::{Client, Credentials, ImageRef, LayerId, PullEvent, Pulled};

pub mod image;
pub use image::{
    Arch, DiscoveredImage, Discovery, ImageConfig, Key, LoadedImage, LocalImage, Manifest, Rootfs,
    Sha256Digest, Store,
};

pub mod build;
pub use build::{BuildCache, CacheId, Command, Dockerfile, Instruction};
