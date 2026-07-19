//! The image → rootfs flow: pull an OCI image into a local store as an unpacked **rootfs**, detect its
//! target arch, and expose its config. `hl-images` is runtime-agnostic — it produces a rootfs + [`Arch`]
//! + config that the CALLER hands to a runtime (e.g. `hl-jit`); it does not depend on any runtime crate.
//!
//! ```no_run
//! let img = hl_images::Store::new("/var/lib/hl/images")
//!     .pull("alpine", "latest", hl_images::Credentials::none(), &mut |_| {})?;
//! // img.rootfs is an unpacked filesystem; img.arch is its target; hand both to your runtime:
//! println!("rootfs {:?} arch {:?} cmd {:?}", img.rootfs, img.arch, img.entrypoint_cmd(["/bin/sh"]));
//! # Ok::<(), hl_images::Error>(())
//! ```
//!
//! The flow is split into cohesive files: the [`Arch`] target enum, the [`LocalImage`] entity, the
//! [`Store`] service, and small config helpers. They share one flat namespace via `use super::*` +
//! the re-globs below.

mod arch;
pub(crate) mod archive;
mod config;
pub(crate) mod digest;
mod discovery;
mod local_image;
mod manifest;
mod store;

pub use arch::Arch;
pub use archive::LoadedImage;
pub use config::{ImageConfig, Key};
pub use digest::Sha256Digest;
pub use discovery::{DiscoveredImage, Discovery, Rootfs};
pub use local_image::LocalImage;
pub use manifest::Manifest;
pub use store::Store;
