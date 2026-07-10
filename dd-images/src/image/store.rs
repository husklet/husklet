//! The local image store service: resolve rootfs paths and pull images into unpacked rootfs trees.

use super::*;
use crate::registry::{Client, Credentials, ImageRef, PullEvent};
use crate::Error;
use std::path::PathBuf;

/// A local image store: a directory holding one `<safe-name>/rootfs` tree per pulled image.
#[derive(Clone, Debug)]
pub struct Store {
    /// The store root; readable by sibling services in this module (e.g. the archive/load path).
    pub(super) dir: String,
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
    ) -> Result<LocalImage, Error> {
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
    ) -> Result<LocalImage, Error> {
        let iref = image_ref(from, tag);
        let rootfs = self.rootfs_path(&iref);
        let pulled = Client::new(iref.clone(), creds).pull(&rootfs, archs, progress)?;
        // Map the config's os/arch; a PRESENT but unsupported os yields `None` from `arch_from_config`
        // (finding 9). Only fall back to the linux/arm64 default when the os is acceptable (absent/empty/
        // linux/darwin) but the arch simply couldn't be classified — never for an explicitly-unsupported os.
        let arch = match arch_from_config(&pulled.config) {
            Some(a) => a,
            None => match pulled.config["os"].as_str() {
                Some(os) if !os.is_empty() && os != "linux" && os != "darwin" => {
                    return Err(Error::Manifest(format!("unsupported image os: {os}")));
                }
                _ => Arch::LinuxAarch64,
            },
        };
        Ok(LocalImage { rootfs, arch, config: pulled.config, iref })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rootfs_path_uses_canonical_safe_name_layout() {
        let store = Store::new("/var/lib/dd/images");
        let iref = ImageRef::parse("nginx");
        // safe_name(nginx) == canonical "docker.io/library/nginx:latest" percent-encoded (`/`->%2F, `:`->%3A).
        assert_eq!(
            store.rootfs_path(&iref),
            PathBuf::from("/var/lib/dd/images/docker.io%2Flibrary%2Fnginx%3Alatest/rootfs")
        );
    }
}
