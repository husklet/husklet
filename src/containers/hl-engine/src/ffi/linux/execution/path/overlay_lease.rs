use std::fs::File;

use hl_runtime::GuestPath;

/// Rootfs layer that supplied the parent observed during a confined walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectedLayer {
    Upper,
    Lower(usize),
}

/// Owned parent capabilities for an overlay path operation.
///
/// A lower hit retains both identities: reads continue through the selected
/// lower parent, while a mutation is issued only through the corresponding
/// upper parent after copy-up has materialized its ancestors. The guest path
/// prevents a host descriptor from becoming the path's namespace identity.
pub(super) struct ParentLease {
    guest: GuestPath,
    selected: File,
    upper: Option<File>,
    layer: SelectedLayer,
}

impl ParentLease {
    pub(super) fn upper(guest: GuestPath, parent: File) -> Self {
        Self {
            guest,
            selected: parent,
            upper: None,
            layer: SelectedLayer::Upper,
        }
    }

    pub(super) fn lower(guest: GuestPath, layer: usize, selected: File, upper: File) -> Self {
        Self {
            guest,
            selected,
            upper: Some(upper),
            layer: SelectedLayer::Lower(layer),
        }
    }

    pub(super) fn guest(&self) -> &GuestPath {
        &self.guest
    }

    pub(super) const fn layer(&self) -> SelectedLayer {
        self.layer
    }

    pub(super) const fn selected(&self) -> &File {
        &self.selected
    }

    pub(super) fn mutation(&self) -> &File {
        self.upper.as_ref().unwrap_or(&self.selected)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::GuestPath;

    use super::{ParentLease, SelectedLayer};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct Directories {
        root: PathBuf,
        lower: File,
        upper: File,
    }

    impl Directories {
        fn new() -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("hl_overlay_lease_{}_{}", std::process::id(), sequence));
            let lower_path = root.join("lower");
            let upper_path = root.join("upper");
            fs::create_dir_all(&lower_path).unwrap();
            fs::create_dir_all(&upper_path).unwrap();
            Self {
                root,
                lower: File::open(lower_path).unwrap(),
                upper: File::open(upper_path).unwrap(),
            }
        }
    }

    impl Drop for Directories {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn lower_selection_retains_upper_mutation_parent() {
        let directories = Directories::new();
        let lease = ParentLease::lower(
            GuestPath::new("/var/cache").unwrap(),
            2,
            directories.lower.try_clone().unwrap(),
            directories.upper.try_clone().unwrap(),
        );

        assert_eq!(lease.guest().as_str(), "/var/cache");
        assert_eq!(lease.layer(), SelectedLayer::Lower(2));
        assert_ne!(lease.selected().as_raw_fd(), lease.mutation().as_raw_fd());
    }

    #[test]
    fn upper_selection_mutates_selected_parent() {
        let directories = Directories::new();
        let lease = ParentLease::upper(GuestPath::new("/etc").unwrap(), directories.upper.try_clone().unwrap());

        assert_eq!(lease.layer(), SelectedLayer::Upper);
        assert_eq!(lease.selected().as_raw_fd(), lease.mutation().as_raw_fd());
    }
}
