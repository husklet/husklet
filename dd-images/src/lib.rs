//! `dd-images` — image handling for dd, kept separate from the container runtime (`dd-jit`) and the
//! Docker polyfill (`dd-daemon`). Today it owns the OCI **registry client** ([`registry`]): pulling and
//! pushing manifests + layers, registry auth, and image references. Image/rootfs *building* (Dockerfile
//! execution, layer extraction) will consolidate here too as it is decoupled from the daemon runtime.
pub mod registry;
pub use registry::{layer_short, Client, Credentials, ImageRef, PullEvent, Pulled};

pub mod image;
pub use image::{arch_from_config, config_strs, image_ref, safe_name, Arch, LocalImage, Store};
