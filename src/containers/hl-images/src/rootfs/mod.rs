use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{
    Error, LeaseStore, Leases, Result,
    error::At as _,
    snapshot::{Id, Snapshots, View as SnapshotView},
};

mod tree;
use tree::{Changes, Tree};
mod executable_digest;
pub use executable_digest::{ExecutableDigest, ExecutableDigestAuthority};

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

    #[test]
    fn overlay_reuses_only_lower_after_fresh_lease_validation() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let leases = Leases::open(root.path().join("metadata")).unwrap();
        let lower = Id::new("lower-reuse").unwrap();
        snapshots
            .prepare(lower.clone(), None)
            .unwrap()
            .commit(lower.clone())
            .unwrap();
        let roots = Roots::new(snapshots.clone(), leases.clone());
        let reference = roots.fork_overlay(&lower).unwrap();
        let views_before = snapshots.view_open_count();
        let leases_before = leases.get_count();

        roots.open_overlay(&reference).unwrap();
        roots.open_overlay(&reference).unwrap();

        // First open loads lower+upper; second loads only the always-fresh upper.
        assert_eq!(snapshots.view_open_count() - views_before, 3);
        assert_eq!(leases.get_count() - leases_before, 2);
    }

    #[test]
    fn cached_lower_never_bypasses_missing_lease_or_fresh_upper() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let leases = Leases::open(root.path().join("metadata")).unwrap();
        let lower = Id::new("lower-refusal").unwrap();
        snapshots
            .prepare(lower.clone(), None)
            .unwrap()
            .commit(lower.clone())
            .unwrap();
        let roots = Roots::new(snapshots.clone(), leases.clone());
        let reference = roots.fork_overlay(&lower).unwrap();
        roots.open_overlay(&reference).unwrap();
        let after_cached = snapshots.view_open_count();
        roots.open_overlay(&reference).unwrap();
        assert_eq!(snapshots.view_open_count(), after_cached + 1, "upper was not reopened");

        leases.delete(reference.lease_id()).unwrap();
        let error = roots.open_overlay(&reference).unwrap_err();
        assert!(matches!(error, Error::NotOwned { .. }));
        assert_eq!(
            snapshots.view_open_count(),
            after_cached + 1,
            "cache was consulted after lease refusal"
        );
    }

    #[test]
    fn lower_cache_is_bounded_clears_with_manager_and_coalesces_concurrent_misses() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let leases = Leases::open(root.path().join("metadata")).unwrap();
        let mut ids = Vec::new();
        for ordinal in 0..=LowerViews::CAPACITY {
            let id = Id::new(format!("bounded-{ordinal}")).unwrap();
            snapshots.prepare(id.clone(), None).unwrap().commit(id.clone()).unwrap();
            ids.push(id);
        }
        let roots = Roots::new(snapshots.clone(), leases.clone());
        for id in &ids {
            roots.lower_view(id).unwrap();
        }
        let after_fill = snapshots.view_open_count();
        roots.lower_view(&ids[0]).unwrap();
        assert_eq!(
            snapshots.view_open_count(),
            after_fill + 1,
            "oldest entry was not evicted"
        );

        let restarted = Roots::new(snapshots.clone(), leases);
        restarted.lower_view(&ids[1]).unwrap();
        assert_eq!(
            snapshots.view_open_count(),
            after_fill + 2,
            "new manager inherited old cache"
        );

        let concurrent = Arc::new(Roots::new(
            snapshots.clone(),
            Leases::open(root.path().join("other-meta")).unwrap(),
        ));
        let before_concurrent = snapshots.view_open_count();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let roots = Arc::clone(&concurrent);
                let barrier = Arc::clone(&barrier);
                let id = ids[2].clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    roots.lower_view(&id).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(snapshots.view_open_count(), before_concurrent + 1);
    }

    /// Forks are reflinked, not fsynced; a write through one must still never reach
    /// the shared parent or a sibling fork.
    #[test]
    fn copied_forks_stay_private_from_the_parent_and_each_other() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let parent = Id::new("parent").unwrap();
        let draft = snapshots.prepare(parent.clone(), None).unwrap();
        std::fs::write(draft.path().join("value"), b"base").unwrap();
        draft.commit(parent.clone()).unwrap();
        let roots = Roots::new(snapshots, Leases::open(root.path().join("metadata")).unwrap());

        let first = roots.fork(&parent).unwrap();
        let second = roots.fork(&parent).unwrap();
        let first_view = roots.open(&first).unwrap();
        let second_view = roots.open(&second).unwrap();
        assert_eq!(std::fs::read(first_view.path().join("value")).unwrap(), b"base");
        assert_eq!(std::fs::read(second_view.path().join("value")).unwrap(), b"base");

        std::fs::write(first_view.path().join("value"), b"mutated").unwrap();
        assert_eq!(std::fs::read(first_view.path().join("value")).unwrap(), b"mutated");
        assert_eq!(std::fs::read(second_view.path().join("value")).unwrap(), b"base");
        let parent_path = roots.snapshots.view(&parent).unwrap().path().join("value");
        assert_eq!(std::fs::read(parent_path).unwrap(), b"base");

        roots.release(&first).unwrap();
        roots.release(&second).unwrap();
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
    lower_views: Arc<Mutex<LowerViews>>,
}

/// Immutable lower snapshots are shared by many container overlays. Keeping a small process-local
/// window avoids reparsing their durable ownership/name sidecars on every exec without retaining an
/// unbounded image working set. Lease ownership is deliberately not cached here: every caller must
/// refresh and validate its lease before consulting this cache.
#[derive(Debug, Default)]
struct LowerViews {
    views: HashMap<Id, SnapshotView>,
    recency: VecDeque<Id>,
}

impl LowerViews {
    const CAPACITY: usize = 16;

    fn get(&mut self, id: &Id) -> Option<SnapshotView> {
        let view = self.views.get(id).cloned()?;
        self.touch(id);
        Some(view)
    }

    fn insert(&mut self, id: Id, view: SnapshotView) {
        self.views.insert(id.clone(), view);
        self.touch(&id);
        while self.views.len() > Self::CAPACITY {
            let Some(expired) = self.recency.pop_front() else {
                break;
            };
            self.views.remove(&expired);
        }
    }

    fn touch(&mut self, id: &Id) {
        self.recency.retain(|candidate| candidate != id);
        self.recency.push_back(id.clone());
    }
}

impl Roots {
    #[must_use]
    pub fn new(snapshots: Snapshots, leases: Leases) -> Self {
        Self {
            snapshots,
            leases,
            lower_views: Arc::new(Mutex::new(LowerViews::default())),
        }
    }

    fn lower_view(&self, id: &Id) -> Result<SnapshotView> {
        let mut cache = self
            .lower_views
            .lock()
            .map_err(|_| Error::InvalidMetadata("lower snapshot cache lock poisoned".into()))?;
        if let Some(view) = cache.get(id) {
            return Ok(view);
        }
        // Hold the lock through the synchronous open so concurrent callers of the same ID cannot
        // duplicate its expensive sidecar load. Distinct cold IDs are rare and bounded by capacity.
        let view = self.snapshots.view(id)?;
        cache.insert(id.clone(), view.clone());
        Ok(view)
    }

    /// Construct the digest authority owned by an immutable snapshot.
    #[must_use]
    pub fn executable_digest_authority(&self, snapshot: &Id) -> ExecutableDigestAuthority {
        ExecutableDigestAuthority::new(
            snapshot.as_str(),
            self.snapshots.root().join("committed").join(snapshot.as_str()),
            self.snapshots.root().join("executable-digests"),
        )
    }

    /// # Errors
    /// Returns an error when the snapshot is absent or durable lease creation fails.
    pub fn pin(&self, snapshot: &Id) -> Result<Reference> {
        self.snapshots.view(snapshot)?;
        self.pin_committed(snapshot)
    }

    /// Pin a snapshot whose publication was completed by this operation.
    ///
    /// Callers must retain the successful `Draft::commit` result through this
    /// call. External snapshot identifiers must use `pin`, which validates all
    /// persisted metadata before creating ownership.
    fn pin_committed(&self, snapshot: &Id) -> Result<Reference> {
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
        let committed = draft.commit(id.clone())?;
        match self.pin_committed(&id) {
            Ok(mut reference) => {
                // Keep the commit's validated view alive until durable ownership
                // has been established; reopening it here would only repeat the
                // publication and sidecar reads performed by `commit`.
                drop(committed);
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
                drop(committed);
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
        let committed = self.snapshots.prepare(upper.clone(), None)?.commit(upper.clone())?;
        let mut reference = match self.pin_committed(&upper) {
            Ok(reference) => reference,
            Err(error) => {
                drop(committed);
                let _ = self.snapshots.remove(&upper);
                return Err(error);
            }
        };
        drop(committed);
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
        std::fs::create_dir_all(&work).at(&work)?;
        Ok(OverlayView {
            reference: reference.clone(),
            lower: self.lower_view(&overlay.lower)?,
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
