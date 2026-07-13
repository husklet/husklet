//! [`Image`] — a container image: a rootfs (optionally an overlay of read-only lower layers) plus the
//! guest personality (OS + ISA) the engine runs it as.

use hl_jit_darwin::Guest;

/// A container image: a rootfs (optionally an overlay of read-only lower layers) plus the guest
/// personality (OS + ISA) the engine runs it as.
#[derive(Clone, Debug)]
pub struct Image {
    pub(crate) rootfs: String,
    pub(crate) lowers: Vec<String>,
    pub(crate) guest: Guest,
}

impl Image {
    /// An image backed by a single rootfs directory. The guest personality defaults to the native
    /// Linux/aarch64 guest; use [`Image::guest`] to override (e.g. an x86-64 or macOS image).
    pub fn from_rootfs(rootfs: impl Into<String>) -> Self {
        Image { rootfs: rootfs.into(), lowers: Vec::new(), guest: Guest::default() }
    }

    /// An overlay image: a writable upper `rootfs` over read-only `lowers` (OCI image layers).
    pub fn overlay(rootfs: impl Into<String>, lowers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Image {
            rootfs: rootfs.into(),
            lowers: lowers.into_iter().map(Into::into).collect(),
            guest: Guest::default(),
        }
    }

    /// Set the guest personality (OS + ISA) this image runs as.
    pub fn guest(mut self, g: Guest) -> Self {
        self.guest = g;
        self
    }

    /// The guest personality this image runs as.
    pub fn guest_of(&self) -> Guest {
        self.guest
    }
}
