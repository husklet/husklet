//! The image → rootfs flow: pull an OCI image into a local store as an unpacked **rootfs**, detect its
//! target arch, and expose its config. `dd-images` is runtime-agnostic — it produces a rootfs + [`Arch`]
//! + config that the CALLER hands to a runtime (e.g. `dd-jit`); it does not depend on any runtime crate.
//!
//! ```no_run
//! let img = dd_images::Store::new("/var/lib/dd/images")
//!     .pull("alpine", "latest", dd_images::Credentials::none(), &mut |_| {})?;
//! // img.rootfs is an unpacked filesystem; img.arch is its target; hand both to your runtime:
//! println!("rootfs {:?} arch {:?} cmd {:?}", img.rootfs, img.arch, img.entrypoint_cmd(["/bin/sh"]));
//! # Ok::<(), String>(())
//! ```
//!
//! The flow is split into cohesive files: the [`Arch`] target enum, the [`LocalImage`] entity, the
//! [`Store`] service, and small config helpers. They share one flat namespace via `use super::*` +
//! the re-globs below.

mod arch;
mod archive;
mod config;
pub(crate) mod digest;
mod discovery;
mod local_image;
mod manifest;
mod store;

pub use arch::{arch_from_config, Arch};
pub use archive::LoadedImage;
pub use config::{
    config_exposed_ports, config_labels, config_stop_signal, config_strs, config_volumes,
    default_shell, image_ref, ref_name, ref_repo, ref_tag, repo_tag, safe_name,
};
pub use discovery::{detect_arch, discover_images, image_score, DiscoveredImage};
pub use local_image::LocalImage;
pub use manifest::Manifest;
pub use store::Store;
