//! `docker ps --size` accounting: the SizeRw/SizeRootFs on-disk `du` walk (`container_sizes`).
use super::*;

/// `docker ps --size` -> (SizeRw, SizeRootFs). hl gives each container a private copy-on-write UPPER over
/// the read-only image rootfs, so SizeRw is the `du`-style size of that writable upper layer (matching
/// docker, which measures the container's writable diff) and SizeRootFs is the full image rootfs walk.
/// The host-fs `macos` image (rootfs "/") is skipped -- walking it would be catastrophic, exactly as
/// `image_size` guards against.
pub(crate) fn container_sizes(c: &Container) -> (i64, i64) {
    if c.image == "macos" || c.rootfs.is_empty() || c.rootfs == "/" {
        return (0, 0);
    }
    let rw = if c.upper.is_empty() {
        0
    } else {
        dir_size(std::path::Path::new(&c.upper))
    };
    (rw, dir_size(std::path::Path::new(&c.rootfs)))
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
        assert_eq!(container_sizes(&c), (0, 0));
        c.image = "nginx".into();
        c.rootfs = "".into();
        assert_eq!(container_sizes(&c), (0, 0));
        c.rootfs = "/".into();
        assert_eq!(container_sizes(&c), (0, 0));
    }
}
