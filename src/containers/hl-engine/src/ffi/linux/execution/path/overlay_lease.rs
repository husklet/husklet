use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_runtime::GuestPathBytes;

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
#[derive(Debug)]
pub(super) struct ParentLease {
    guest: Option<GuestPathBytes>,
    selected: OwnedFd,
    upper: Option<OwnedFd>,
    lower_parents: Vec<OwnedFd>,
    upper_root: Option<OwnedFd>,
    epoch: Option<Arc<AtomicU64>>,
    layer: SelectedLayer,
}

impl ParentLease {
    pub(super) fn upper(guest: GuestPathBytes, parent: OwnedFd) -> Self {
        Self {
            guest: Some(guest),
            selected: parent,
            upper: None,
            lower_parents: Vec::new(),
            upper_root: None,
            epoch: None,
            layer: SelectedLayer::Upper,
        }
    }

    pub(super) fn lower(guest: GuestPathBytes, layer: usize, selected: OwnedFd, upper: Option<OwnedFd>) -> Self {
        Self {
            guest: Some(guest),
            selected,
            upper,
            lower_parents: Vec::new(),
            upper_root: None,
            epoch: None,
            layer: SelectedLayer::Lower(layer),
        }
    }

    pub(super) fn guest(&self) -> Option<&GuestPathBytes> {
        self.guest.as_ref()
    }

    pub(super) const fn layer(&self) -> SelectedLayer {
        self.layer
    }

    pub(super) const fn selected(&self) -> &OwnedFd {
        &self.selected
    }

    pub(super) fn mutation(&self) -> Result<&OwnedFd, hl_runtime::RuntimePathError> {
        match self.layer {
            SelectedLayer::Upper => Ok(&self.selected),
            SelectedLayer::Lower(_) => self.upper.as_ref().ok_or(hl_runtime::RuntimePathError::NotFound),
        }
    }

    pub(super) fn with_lower_parents(mut self, lower_parents: Vec<OwnedFd>) -> Self {
        self.lower_parents = lower_parents;
        self
    }

    pub(super) fn lower_parents(&self) -> &[OwnedFd] {
        &self.lower_parents
    }

    pub(super) fn with_upper_root(mut self, upper_root: OwnedFd) -> Self {
        self.upper_root = Some(upper_root);
        self
    }

    pub(super) fn upper_root(&self) -> Option<&OwnedFd> {
        self.upper_root.as_ref()
    }

    pub(super) fn install_upper(&mut self, upper: OwnedFd) {
        self.upper = Some(upper);
    }

    pub(super) fn with_epoch(mut self, epoch: Arc<AtomicU64>) -> Self {
        self.epoch = Some(epoch);
        self
    }

    pub(super) fn publish(&self) {
        if let Some(epoch) = &self.epoch {
            epoch.fetch_add(1, Ordering::Release);
        }
    }
}

impl From<OwnedFd> for ParentLease {
    fn from(selected: OwnedFd) -> Self {
        Self {
            guest: None,
            selected,
            upper: None,
            lower_parents: Vec::new(),
            upper_root: None,
            epoch: None,
            layer: SelectedLayer::Upper,
        }
    }
}

impl AsRawFd for ParentLease {
    fn as_raw_fd(&self) -> RawFd {
        self.mutation().map_or(-1, AsRawFd::as_raw_fd)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use hl_runtime::GuestPathBytes;

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
            GuestPathBytes::new(b"/var/cache").unwrap(),
            2,
            directories.lower.try_clone().unwrap().into(),
            Some(directories.upper.try_clone().unwrap().into()),
        );

        assert_eq!(lease.guest().unwrap().as_bytes(), b"/var/cache");
        assert_eq!(lease.layer(), SelectedLayer::Lower(2));
        assert_ne!(lease.selected().as_raw_fd(), lease.mutation().unwrap().as_raw_fd());
    }

    #[test]
    fn upper_selection_mutates_selected_parent() {
        let directories = Directories::new();
        let lease = ParentLease::upper(
            GuestPathBytes::new(b"/etc").unwrap(),
            directories.upper.try_clone().unwrap().into(),
        );

        assert_eq!(lease.layer(), SelectedLayer::Upper);
        assert_eq!(lease.selected().as_raw_fd(), lease.mutation().unwrap().as_raw_fd());
    }

    #[test]
    fn publication_advances_shared_epoch() {
        let directories = Directories::new();
        let epoch = Arc::new(AtomicU64::new(7));
        let lease = ParentLease::upper(
            GuestPathBytes::new(b"/").unwrap(),
            directories.upper.try_clone().unwrap().into(),
        )
        .with_epoch(Arc::clone(&epoch));
        lease.publish();
        assert_eq!(epoch.load(Ordering::Acquire), 8);
    }
}
