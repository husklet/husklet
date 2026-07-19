//! `docker ps --size` accounting: the SizeRw/SizeRootFs on-disk `du` walk (`container_sizes`).
use super::*;

/// `docker ps --size` -> (SizeRw, SizeRootFs). hl gives each container a private copy-on-write UPPER over
/// the read-only image rootfs, so SizeRw is the `du`-style size of that writable upper layer (matching
/// docker, which measures the container's writable diff) and SizeRootFs is the full image rootfs walk.
/// The host-fs `macos` image (rootfs "/") is skipped -- walking it would be catastrophic, exactly as
/// `image_size` guards against.
impl Container {
    pub(crate) fn sizes(&self) -> (i64, i64) {
        if self.image == "macos" || self.rootfs.is_empty() || self.rootfs == "/" {
            return (0, 0);
        }
        let rw = if self.upper.is_empty() {
            0
        } else {
            PathSize::size(std::path::Path::new(&self.upper))
        };
        (rw, PathSize::size(std::path::Path::new(&self.rootfs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_sizes_guards_return_zero_without_touching_fs() {
        // The catastrophic-walk guards: host-fs `macos`, empty rootfs, and rootfs "/" all short-circuit.
        let mut c = ctr();
        c.image = "macos".into();
        c.rootfs = "/".into();
        assert_eq!(c.sizes(), (0, 0));
        c.image = "nginx".into();
        c.rootfs = "".into();
        assert_eq!(c.sizes(), (0, 0));
        c.rootfs = "/".into();
        assert_eq!(c.sizes(), (0, 0));
    }
}
