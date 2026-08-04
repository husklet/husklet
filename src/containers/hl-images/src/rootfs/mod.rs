use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    Error, LeaseStore, Leases, Result,
    snapshot::{Id, Snapshots, View as SnapshotView},
};

mod tree;
use tree::{Changes, Tree};

/// Durable identity of a pinned root filesystem. Persist this with container metadata.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    version: u16,
    snapshot: Id,
    baseline: Option<Id>,
    lease: String,
    owned: bool,
    overlay: Option<OverlayReference>,
}

impl Reference {
    const fn version() -> u16 {
        1
    }
}

/// Durable identities for an overlay root. The lower tree remains immutable;
/// the upper and work trees are private to this reference.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayReference {
    lower: Id,
    upper: Id,
    work: String,
}
impl OverlayReference {
    #[must_use]
    pub fn lower(&self) -> &Id {
        &self.lower
    }
    #[must_use]
    pub fn upper(&self) -> &Id {
        &self.upper
    }
}

impl Reference {
    #[must_use]
    pub fn snapshot(&self) -> &Id {
        &self.snapshot
    }
    #[must_use]
    pub fn lease_id(&self) -> &str {
        &self.lease
    }
    #[must_use]
    pub fn overlay(&self) -> Option<&OverlayReference> {
        self.overlay.as_ref()
    }
    fn resource(&self) -> String {
        format!("snapshot:{}", self.snapshot.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_fork_keeps_lower_immutable_and_creates_empty_upper() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let lower = Id::new("lower").unwrap();
        let draft = snapshots.prepare(lower.clone(), None).unwrap();
        std::fs::write(draft.path().join("value"), b"lower").unwrap();
        draft.commit(lower.clone()).unwrap();
        let roots = Roots::new(snapshots, Leases::open(root.path().join("metadata")).unwrap());
        let reference = roots.fork_overlay(&lower).unwrap();
        let view = roots.open_overlay(&reference).unwrap();
        assert_eq!(std::fs::read(view.lower().join("value")).unwrap(), b"lower");
        assert!(!view.upper().join("value").exists());
        assert!(view.work().is_dir());
        roots.release(&reference).unwrap();
        assert!(!view.work().exists());
    }
}

/// A validated view of a pinned rootfs. Dropping it does not release durable ownership.
#[derive(Clone, Debug)]
pub struct View {
    reference: Reference,
    view: SnapshotView,
}

#[derive(Clone, Debug)]
pub struct OverlayView {
    reference: Reference,
    lower: SnapshotView,
    upper: SnapshotView,
    work: PathBuf,
}

/// Runtime-neutral classification of a rootfs change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
}

/// One guest-visible path changed relative to an image baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}
impl OverlayView {
    #[must_use]
    pub fn reference(&self) -> &Reference {
        &self.reference
    }
    #[must_use]
    pub fn lower(&self) -> &Path {
        self.lower.path()
    }
    #[must_use]
    pub fn upper(&self) -> &Path {
        self.upper.path()
    }
    #[must_use]
    pub fn work(&self) -> &Path {
        &self.work
    }
    /// Immutable ownership metadata inherited from the lower snapshot.
    #[must_use]
    pub fn lower_ownership(&self) -> &crate::snapshot::Ownerships {
        self.lower.ownership()
    }
    /// Private ownership deltas recorded in the upper snapshot.
    #[must_use]
    pub fn upper_ownership(&self) -> &crate::snapshot::Ownerships {
        self.upper.ownership()
    }
    #[must_use]
    pub fn lower_names(&self) -> &crate::snapshot::Names {
        self.lower.names()
    }
    #[must_use]
    pub fn upper_names(&self) -> &crate::snapshot::Names {
        self.upper.names()
    }

    /// Archive only the writable overlay layer with guest names and ownership metadata.
    /// The immutable lower tree is never traversed or materialized.
    ///
    /// Overlay whiteout and opaque-directory marker files are retained verbatim for OCI layer
    /// application.
    ///
    /// # Errors
    /// Returns traversal, name, ownership, or archive write failures.
    pub fn archive_upper(&self, writer: impl std::io::Write) -> Result<()> {
        self.upper.archive(writer)
    }
}
impl View {
    #[must_use]
    pub fn path(&self) -> &Path {
        self.view.path()
    }
    #[must_use]
    pub fn reference(&self) -> &Reference {
        &self.reference
    }
    #[must_use]
    pub fn ownership(&self) -> &crate::snapshot::Ownerships {
        self.view.ownership()
    }
    #[must_use]
    pub fn names(&self) -> &crate::snapshot::Names {
        self.view.names()
    }
}

/// Creates, reopens, and explicitly releases durable rootfs ownership.
#[derive(Clone, Debug)]
pub struct Roots {
    snapshots: Snapshots,
    leases: Leases,
}
impl Roots {
    #[must_use]
    pub fn new(snapshots: Snapshots, leases: Leases) -> Self {
        Self { snapshots, leases }
    }

    /// # Errors
    /// Returns an error when the snapshot is absent or durable lease creation fails.
    pub fn pin(&self, snapshot: &Id) -> Result<Reference> {
        self.snapshots.view(snapshot)?;
        let resource = format!("snapshot:{}", snapshot.as_str());
        let lease = self.leases.create_with(
            BTreeMap::from([
                ("kind".into(), "rootfs".into()),
                ("snapshot".into(), snapshot.as_str().into()),
            ]),
            [resource],
        )?;
        let reference = Reference {
            version: Reference::version(),
            snapshot: snapshot.clone(),
            baseline: None,
            lease: lease.id().into(),
            owned: false,
            overlay: None,
        };
        Ok(reference)
    }

    /// Creates a private writable snapshot derived from `parent` and pins its ownership.
    ///
    /// # Errors
    /// Returns an error when the parent is absent or snapshot/lease persistence fails.
    pub fn fork(&self, parent: &Id) -> Result<Reference> {
        let id = Id::new(format!("container-{}", uuid::Uuid::new_v4().simple()))?;
        let draft = self.snapshots.prepare(id.clone(), Some(parent))?;
        draft.commit(id.clone())?;
        match self.pin(&id) {
            Ok(mut reference) => {
                reference.baseline = Some(parent.clone());
                if let Err(error) = self
                    .leases
                    .add(reference.lease_id(), format!("snapshot:{}", parent.as_str()))
                {
                    let _ = self.leases.delete(reference.lease_id());
                    let _ = self.snapshots.remove(&id);
                    return Err(error);
                }
                reference.owned = true;
                Ok(reference)
            }
            Err(error) => {
                let _ = self.snapshots.remove(&id);
                Err(error)
            }
        }
    }

    /// Create an empty private upper/work pair over an immutable lower snapshot.
    /// This is opt-in until the selected engine validates generic overlay roots.
    ///
    /// # Errors
    /// Returns an error when the lower is absent or upper/lease publication fails.
    pub fn fork_overlay(&self, parent: &Id) -> Result<Reference> {
        self.snapshots.view(parent)?;
        let upper = Id::new(format!("upper-{}", uuid::Uuid::new_v4().simple()))?;
        self.snapshots.prepare(upper.clone(), None)?.commit(upper.clone())?;
        let mut reference = self.pin(&upper)?;
        let work = format!("work-{}", uuid::Uuid::new_v4().simple());
        reference.baseline = Some(parent.clone());
        reference.owned = true;
        reference.overlay = Some(OverlayReference {
            lower: parent.clone(),
            upper: upper.clone(),
            work,
        });
        if let Err(error) = self
            .leases
            .add(reference.lease_id(), format!("snapshot:{}", parent.as_str()))
        {
            let _ = self.leases.delete(reference.lease_id());
            let _ = self.snapshots.remove(&upper);
            return Err(error);
        }
        Ok(reference)
    }

    /// Open and validate both sides of an overlay reference.
    ///
    /// # Errors
    /// Returns an error for copied references, missing leases, or missing snapshots.
    pub fn open_overlay(&self, reference: &Reference) -> Result<OverlayView> {
        let overlay = reference
            .overlay
            .as_ref()
            .ok_or_else(|| Error::InvalidMetadata("rootfs reference is not an overlay".into()))?;
        let lease = self
            .leases
            .get(reference.lease_id())?
            .ok_or_else(|| reference.not_owned())?;
        for id in [&overlay.lower, &overlay.upper] {
            if !lease.owns(&format!("snapshot:{}", id.as_str())) {
                return Err(reference.not_owned());
            }
        }
        let work = self.snapshots.root().join("work").join(&overlay.work);
        std::fs::create_dir_all(&work)?;
        Ok(OverlayView {
            reference: reference.clone(),
            lower: self.snapshots.view(&overlay.lower)?,
            upper: self.snapshots.view(&overlay.upper)?,
            work,
        })
    }

    /// # Errors
    /// Returns an error when the durable lease does not own an existing snapshot.
    pub fn open(&self, reference: &Reference) -> Result<View> {
        let lease = self
            .leases
            .get(reference.lease_id())?
            .ok_or_else(|| reference.not_owned())?;
        if !lease.owns(&reference.resource()) {
            return Err(reference.not_owned());
        }
        Ok(View {
            reference: reference.clone(),
            view: self.snapshots.view(reference.snapshot())?,
        })
    }

    /// Opens the immutable snapshot from which a writable rootfs was forked.
    ///
    /// # Errors
    /// Returns an error when the reference predates baseline tracking or its parent is absent.
    pub fn baseline(&self, reference: &Reference) -> Result<SnapshotView> {
        let parent = reference
            .baseline
            .as_ref()
            .ok_or_else(|| Error::InvalidMetadata("rootfs reference has no baseline snapshot".into()))?;
        let lease = self
            .leases
            .get(reference.lease_id())?
            .ok_or_else(|| reference.not_owned())?;
        if !lease.owns(&format!("snapshot:{}", parent.as_str())) {
            return Err(reference.not_owned());
        }
        self.snapshots.view(parent)
    }

    /// Compare a private rootfs with its immutable image baseline.
    ///
    /// Overlay roots are merged logically from lower and upper metadata. Whiteout and opaque
    /// markers are interpreted without copying or mounting the lower tree.
    ///
    /// # Errors
    /// Returns ownership, metadata, traversal, or content-read failures.
    pub fn changes(&self, reference: &Reference) -> Result<Vec<Change>> {
        let baseline = self.baseline(reference)?;
        let before = Tree::read(&baseline)?;
        let after = if reference.overlay.is_some() {
            let overlay = self.open_overlay(reference)?;
            Tree::merge(&overlay.lower, &overlay.upper)?
        } else {
            Tree::read(&self.open(reference)?.view)?
        };
        Changes::between(&before, &after).map(Changes::into_inner)
    }

    /// # Errors
    /// Returns an error when ownership validation or the durable release fails.
    pub fn release(&self, reference: &Reference) -> Result<()> {
        let lease = self
            .leases
            .get(reference.lease_id())?
            .ok_or_else(|| reference.not_owned())?;
        if !lease.owns(&reference.resource()) {
            return Err(reference.not_owned());
        }
        self.leases.delete(reference.lease_id())?;
        if let Some(overlay) = &reference.overlay {
            let _ = std::fs::remove_dir_all(self.snapshots.root().join("work").join(&overlay.work));
        }
        if reference.owned {
            self.snapshots.remove(reference.snapshot())?;
        }
        Ok(())
    }
}

impl Reference {
    fn not_owned(&self) -> Error {
        Error::NotOwned {
            lease: self.lease.clone(),
            resource: self.resource(),
        }
    }
}
