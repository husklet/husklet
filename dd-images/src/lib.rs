//! `dd-images` — image handling for dd, kept separate from the container runtime (`dd-jit`) and the
//! Docker polyfill (`dd-daemon`). Today it owns the OCI **registry client** ([`registry`]): pulling and
//! pushing manifests + layers, registry auth, and image references. Image/rootfs *building* (Dockerfile
//! execution, layer extraction) will consolidate here too as it is decoupled from the daemon runtime.
pub mod registry;
pub use registry::{layer_short, Client, Credentials, ImageRef, PullEvent, Pulled};

pub mod image;
pub use image::{
    arch_from_config, config_exposed_ports, config_labels, config_stop_signal, config_strs,
    config_volumes, default_shell, image_ref, repo_tag, ref_tag, safe_name, Arch, LocalImage, Store,
};

pub mod build;
pub use build::{
    cache_id, is_fs_inst, parse_dockerfile, parse_exec_form, parse_labels, path_digest,
    rootfs_digest, sha256_hex, substitute_args, BuildCache,
};
