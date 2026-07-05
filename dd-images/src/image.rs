//! The image → rootfs → runnable-image flow: pull an OCI image into a local store as an unpacked
//! **rootfs**, detect its guest arch, and hand it to `dd-jit` to run. This is what makes `dd-images`
//! usable on its own — pull here, run with `dd-jit`, no daemon required:
//!
//! ```no_run
//! let img = dd_images::Store::new("/var/lib/dd/images")
//!     .pull("alpine", "latest", dd_images::Credentials::none(), &mut |_| {})?;
//! let c = dd_jit::Container::builder(img.to_jit_image())
//!     .cmd(img.entrypoint_cmd(["/bin/sh", "-c", "echo hi"]))
//!     .build()?;
//! let mut h = dd_jit::Runtime::new()?.run(&c)?;
//! h.wait()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::registry::{Client, Credentials, ImageRef, PullEvent};
use dd_jit::Guest;
use serde_json::Value;
use std::path::PathBuf;

/// A local image store: a directory holding one `<safe-name>/rootfs` tree per pulled image.
#[derive(Clone, Debug)]
pub struct Store {
    dir: String,
}

/// An image pulled into the local store: its unpacked rootfs, detected guest arch, and OCI config blob.
/// Convert it to a [`dd_jit::Image`] with [`LocalImage::to_jit_image`] and read the image's default
/// command/env with the accessors.
#[derive(Clone, Debug)]
pub struct LocalImage {
    /// The unpacked root filesystem — pass this to `dd-jit`.
    pub rootfs: PathBuf,
    /// The guest personality (OS + ISA) detected from the image config.
    pub arch: Guest,
    /// The raw OCI image config blob (`{ architecture, os, config: { Cmd, Entrypoint, Env, … } }`).
    pub config: Value,
    /// The reference this was pulled from.
    pub iref: ImageRef,
}

impl Store {
    /// A store rooted at `dir` (created on demand).
    pub fn new(dir: impl Into<String>) -> Self {
        Store { dir: dir.into() }
    }

    /// The on-disk rootfs path for a reference (whether or not it is present yet).
    pub fn rootfs_path(&self, iref: &ImageRef) -> PathBuf {
        PathBuf::from(format!("{}/{}/rootfs", self.dir, safe_name(iref)))
    }

    /// Pull `from:tag` from its registry and unpack it into the store, preferring the native arm64
    /// variant (falls back to amd64). `progress` receives layer/pull events. Returns the [`LocalImage`].
    pub fn pull(
        &self,
        from: &str,
        tag: &str,
        creds: Credentials,
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<LocalImage, String> {
        self.pull_archs(from, tag, creds, &["arm64", "amd64"], progress)
    }

    /// Like [`pull`](Self::pull) but with an explicit registry arch preference order.
    pub fn pull_archs(
        &self,
        from: &str,
        tag: &str,
        creds: Credentials,
        archs: &[&str],
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<LocalImage, String> {
        let iref = image_ref(from, tag);
        let rootfs = self.rootfs_path(&iref);
        let pulled = Client::new(iref.clone(), creds).pull(&rootfs, archs, progress)?;
        let arch = arch_from_config(&pulled.config).unwrap_or(Guest::LinuxAarch64);
        Ok(LocalImage { rootfs, arch, config: pulled.config, iref })
    }
}

impl LocalImage {
    /// Build the runnable [`dd_jit::Image`] for this rootfs (with its detected guest personality).
    pub fn to_jit_image(&self) -> dd_jit::Image {
        dd_jit::Image::from_rootfs(self.rootfs.to_string_lossy().into_owned()).guest(self.arch)
    }

    /// The image's `Entrypoint` (OCI `config.config.Entrypoint`).
    pub fn entrypoint(&self) -> Vec<String> {
        config_strs(&self.config, "Entrypoint")
    }

    /// The image's default `Cmd` (OCI `config.config.Cmd`).
    pub fn cmd(&self) -> Vec<String> {
        config_strs(&self.config, "Cmd")
    }

    /// The image's `Env` lines (`K=V`).
    pub fn env(&self) -> Vec<String> {
        config_strs(&self.config, "Env")
    }

    /// The image's `WorkingDir` (empty if unset).
    pub fn workdir(&self) -> String {
        self.config["config"]["WorkingDir"].as_str().unwrap_or("").to_string()
    }

    /// The image's default `User` (empty if unset).
    pub fn user(&self) -> String {
        self.config["config"]["User"].as_str().unwrap_or("").to_string()
    }

    /// The effective launch argv: the image `Entrypoint` followed by `override_cmd` if non-empty, else
    /// the image's own `Cmd` (docker's entrypoint/cmd composition). Pass the result to `.cmd(..)`.
    pub fn entrypoint_cmd<S: Into<String>>(&self, override_cmd: impl IntoIterator<Item = S>) -> Vec<String> {
        let mut argv = self.entrypoint();
        let over: Vec<String> = override_cmd.into_iter().map(Into::into).collect();
        if over.is_empty() {
            argv.extend(self.cmd());
        } else {
            argv.extend(over);
        }
        argv
    }
}

/// The store path component for a reference: its canonical form with `/` and `:` flattened to `_`.
pub fn safe_name(r: &ImageRef) -> String {
    r.canonical().replace(['/', ':'], "_")
}

/// Parse `from_image` into an [`ImageRef`], overriding the tag with `tag` when non-empty.
pub fn image_ref(from_image: &str, tag: &str) -> ImageRef {
    let mut r = ImageRef::parse(from_image);
    if !tag.is_empty() {
        r.tag = tag.to_string();
    }
    r
}

/// Map an OCI config blob's `architecture` + `os` to a [`Guest`]. `None` if unrecognized.
pub fn arch_from_config(config: &Value) -> Option<Guest> {
    let os = config["os"].as_str().unwrap_or("linux");
    match (os, config["architecture"].as_str()?) {
        ("darwin", "arm64" | "aarch64") => Some(Guest::DarwinAarch64),
        (_, "amd64" | "x86_64") => Some(Guest::LinuxX86_64),
        (_, "arm64" | "aarch64") => Some(Guest::LinuxAarch64),
        _ => None,
    }
}

/// A string array at `config.config.<key>` of an OCI config blob, flattened to `Vec<String>`.
pub fn config_strs(config: &Value, key: &str) -> Vec<String> {
    config["config"][key]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
