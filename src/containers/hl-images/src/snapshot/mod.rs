use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{Error, Result, error::At as _};

mod draft;
pub use draft::{Draft, View};
mod id;
pub use id::Id;
mod ownership;
pub use ownership::{Ownership, Ownerships};
mod names;
pub use names::Names;
mod archive;
pub(super) mod index;
mod tree;
use tree::Tree;
mod record;
use crate::storage::{Native, Persistence as _};
use fs2::FileExt as _;
pub(crate) use record::LayerRecord;
use record::{DraftOwner, Publication};

#[derive(Clone, Debug)]
pub struct Snapshots {
    root: PathBuf,
}
impl Snapshots {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    /// # Errors
    /// Returns an error when snapshot directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        // Opening a store is not an ownership boundary. Another process can have
        // live drafts in this directory, so startup cleanup here would destroy
        // snapshots underneath running containers. Draft rollback and explicit
        // garbage collection own cleanup instead.
        for directory in [
            "active",
            "committed",
            "ownership/active",
            "ownership/committed",
            "names/active",
            "names/committed",
            "publication/committed",
            "index/committed",
            "drafts",
            "draft-locks",
            "work",
        ] {
            let directory = root.as_ref().join(directory);
            fs::create_dir_all(&directory).at(directory)?;
        }
        let snapshots = Self {
            root: root.as_ref().to_owned(),
        };
        snapshots.recover_abandoned_drafts()?;
        Ok(snapshots)
    }
    /// # Errors
    /// Returns an error for a duplicate key, missing parent, or copy failure.
    pub fn prepare(&self, key: Id, parent: Option<&Id>) -> Result<Draft> {
        let lock_path = self.root.join("draft-locks").join(format!("{}.lock", key.as_str()));
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .at(&lock_path)?;
        lock.lock_exclusive().at(&lock_path)?;
        self.cleanup_active(&key)?;
        Native.replace(
            &self.root.join("drafts").join(format!("{}.json", key.as_str())),
            &serde_json::to_vec(&DraftOwner::active(key.clone()))?,
        )?;
        let path = self.root.join("active").join(key.as_str());
        if let Err(error) = fs::create_dir(&path) {
            let _ = Native.remove(&self.root.join("drafts").join(format!("{}.json", key.as_str())));
            return Err(error).at(path);
        }
        let ownership_path = self.ownership_path("active", &key);
        let names_path = self.names_path("active", &key);
        let ownership = match parent {
            Some(parent) => {
                let parent_path = self.root.join("committed").join(parent.as_str());
                let result = Tree::from(parent_path.as_path())
                    .copy_to(&path)
                    .and_then(|()| Ownerships::fork(&self.ownership_path("committed", parent), ownership_path.clone()))
                    .and_then(|ownership| {
                        Names::fork(&self.names_path("committed", parent), names_path.clone())
                            .map(|names| (ownership, names))
                    });
                if result.is_err() {
                    let _ = fs::remove_dir_all(&path);
                    let _ = fs::remove_file(&ownership_path);
                    let _ = fs::remove_file(&names_path);
                }
                result?
            }
            None => match Ownerships::create(ownership_path.clone())
                .and_then(|ownership| Names::create(names_path.clone()).map(|names| (ownership, names)))
            {
                Ok(metadata) => metadata,
                Err(error) => {
                    let _ = fs::remove_dir_all(&path);
                    let _ = fs::remove_file(&ownership_path);
                    let _ = fs::remove_file(&names_path);
                    return Err(error);
                }
            },
        };
        let (ownership, names) = ownership;
        Ok(Draft {
            path,
            key,
            root: self.root.clone(),
            ownership,
            names,
            finished: false,
            lock,
        })
    }
    /// # Errors
    /// Returns an error when the committed snapshot does not exist.
    pub fn view(&self, id: &Id) -> Result<View> {
        self.publication(id)?;
        let path = self.root.join("committed").join(id.as_str());
        if !path.is_dir() {
            return Err(Error::InvalidMetadata(format!("unknown snapshot {}", id.as_str())));
        }
        Ok(View {
            id: id.clone(),
            path: Arc::new(path),
            ownership: Ownerships::open(self.ownership_path("committed", id))?,
            names: Names::open(self.names_path("committed", id))?,
        })
    }

    /// Check whether a committed snapshot and both metadata sidecars exist.
    /// This is the constant-time cache probe; callers that need metadata use [`Self::view`].
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.root.join("committed").join(id.as_str()).is_dir()
            && self.ownership_path("committed", id).is_file()
            && self.names_path("committed", id).is_file()
            && self.publication(id).is_ok()
    }

    /// Report whether a committed snapshot's ownership sidecar declares no entries.
    ///
    /// A single entry serializes to at least `{"a":{"uid":0,"gid":0}}`, so a shorter
    /// sidecar is an empty map and this stays a stat rather than a parse of the map.
    pub(crate) fn ownership_is_empty(&self, id: &Id) -> Result<bool> {
        const SHORTEST_ENTRY: u64 = 16;

        let path = self.ownership_path("committed", id);
        Ok(fs::metadata(&path).at(&path)?.len() < SHORTEST_ENTRY)
    }

    pub(crate) fn layer_records(&self, id: &Id) -> Result<Option<Vec<LayerRecord>>> {
        Ok(self.publication(id)?.layers())
    }

    pub(crate) fn discard_unpublished(&self, id: &Id) -> Result<()> {
        if !self.contains(id)
            && (self.root.join("committed").join(id.as_str()).exists()
                || self.ownership_path("committed", id).exists()
                || self.names_path("committed", id).exists()
                || self.publication_path(id).exists())
        {
            self.remove(id)?;
        }
        Ok(())
    }

    /// Report whether a committed snapshot has no root entries.
    ///
    /// # Errors
    /// Returns an error when the committed snapshot cannot be enumerated.
    pub(crate) fn is_empty(&self, id: &Id) -> Result<bool> {
        let path = self.root.join("committed").join(id.as_str());
        Ok(fs::read_dir(&path).at(&path)?.next().transpose().at(&path)?.is_none())
    }

    /// List committed snapshots in deterministic identifier order.
    ///
    /// # Errors
    /// Returns an error when the snapshot directory is unreadable or malformed.
    pub fn committed(&self) -> Result<Vec<Id>> {
        let mut snapshots = Vec::new();
        let committed = self.root.join("committed");
        for entry in fs::read_dir(&committed).at(&committed)? {
            let entry = entry.at(&committed)?;
            if entry.file_type().at(entry.path())?.is_dir() {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::InvalidMetadata("non-UTF-8 snapshot identifier".into()))?;
                snapshots.push(Id::new(name)?);
            }
        }
        snapshots.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(snapshots)
    }

    /// Remove a committed snapshot.
    ///
    /// # Errors
    /// Returns an error when the snapshot directory cannot be removed.
    pub fn remove(&self, id: &Id) -> Result<bool> {
        let path = self.root.join("committed").join(id.as_str());
        Tree::from(path.as_path()).writable()?;
        match fs::remove_dir_all(&path) {
            Ok(()) => {
                let ownership = self.ownership_path("committed", id);
                match fs::remove_file(&ownership) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).at(ownership),
                }
                let _ = fs::remove_file(self.names_path("committed", id));
                let _ = Native.remove(&self.publication_path(id));
                index::discard(&self.root, id);
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(self.ownership_path("committed", id));
                let _ = fs::remove_file(self.names_path("committed", id));
                let _ = Native.remove(&self.publication_path(id));
                index::discard(&self.root, id);
                Ok(false)
            }
            Err(error) => Err(error).at(path),
        }
    }

    fn ownership_path(&self, state: &str, id: &Id) -> PathBuf {
        self.root
            .join("ownership")
            .join(state)
            .join(format!("{}.json", id.as_str()))
    }
    fn names_path(&self, state: &str, id: &Id) -> PathBuf {
        self.root
            .join("names")
            .join(state)
            .join(format!("{}.json", id.as_str()))
    }

    /// Path of a committed chain's name-index sidecar, present only for layer chains.
    #[must_use]
    pub fn index_path(&self, id: &Id) -> PathBuf {
        index::path(&self.root, id)
    }

    fn publication_path(&self, id: &Id) -> PathBuf {
        self.root
            .join("publication/committed")
            .join(format!("{}.json", id.as_str()))
    }

    fn publication(&self, id: &Id) -> Result<Publication> {
        const MAX_PUBLICATION_BYTES: u64 = 128 * 1024;
        let path = self.publication_path(id);
        let file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::InvalidMetadata(format!("snapshot {} is not completely published", id.as_str()))
            } else {
                Error::Io {
                    path: Some(path.clone()),
                    source: error,
                }
            }
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_PUBLICATION_BYTES + 1).read_to_end(&mut bytes).at(&path)?;
        if bytes.len() as u64 > MAX_PUBLICATION_BYTES {
            return Err(Error::InvalidMetadata("snapshot publication exceeds 128 KiB".into()));
        }
        let publication = serde_json::from_slice::<Publication>(&bytes)
            .map_err(|error| {
                Error::InvalidMetadata(format!("malformed snapshot publication {}: {error}", path.display()))
            })?
            .validate()?;
        publication.validate_key(id)?;
        Ok(publication)
    }

    fn recover_abandoned_drafts(&self) -> Result<()> {
        let drafts = self.root.join("drafts");
        for entry in fs::read_dir(&drafts).at(&drafts)? {
            let entry = entry.at(&drafts)?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let key = Id::new(name)?;
            let lock_path = self.root.join("draft-locks").join(format!("{name}.lock"));
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .at(&lock_path)?;
            match lock.try_lock_exclusive() {
                Ok(()) => self.recover_draft(&drafts.join(format!("{name}.json")), &key)?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).at(lock_path),
            }
        }
        Ok(())
    }

    fn recover_draft(&self, owner_path: &std::path::Path, key: &Id) -> Result<()> {
        // The owner completed and reclaimed its own record between the scan and this
        // read; there is nothing abandoned to recover.
        let bytes = match fs::read(owner_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).at(owner_path),
        };
        if bytes.len() > 4096 {
            return Err(Error::InvalidMetadata("snapshot draft owner exceeds 4 KiB".into()));
        }
        let target = serde_json::from_slice::<DraftOwner>(&bytes)
            .map_err(|error| Error::InvalidMetadata(format!("malformed snapshot draft owner: {error}")))?
            .validate(key)?;
        self.cleanup_active(key)?;
        if let Some(target) = target
            && !self.contains(&target)
        {
            self.remove(&target)?;
        }
        Ok(())
    }

    fn cleanup_active(&self, key: &Id) -> Result<()> {
        let path = self.root.join("active").join(key.as_str());
        Tree::from(path.as_path()).writable()?;
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).at(path),
        }
        for path in [self.ownership_path("active", key), self.names_path("active", key)] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).at(path),
            }
        }
        let _ = Native.remove(&self.root.join("drafts").join(format!("{}.json", key.as_str())))?;
        Ok(())
    }
}

#[cfg(test)]
mod publication_tests {
    use super::{DraftOwner, Id, LayerRecord, Snapshots};
    use crate::{Digest, layer::DiffSize};

    fn records(count: usize) -> Vec<LayerRecord> {
        let mut records = Vec::new();
        let mut parent = None;
        for index in 0..count {
            let diff_id = Digest::sha256(format!("layer-{index}").as_bytes());
            let chain_id = parent.as_ref().map_or_else(
                || diff_id.clone(),
                |parent| Digest::sha256(format!("{parent} {diff_id}").as_bytes()),
            );
            records.push(
                LayerRecord::new(
                    diff_id,
                    parent.clone(),
                    chain_id.clone(),
                    DiffSize::new(index as u64 + 1),
                )
                .unwrap(),
            );
            parent = Some(chain_id);
        }
        records
    }

    fn id(records: &[LayerRecord]) -> Id {
        records.last().map_or_else(
            || Id::new("chain-empty").unwrap(),
            |record| Id::new(format!("chain-{}", record.chain_id.encoded())).unwrap(),
        )
    }

    fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn walk(root: &std::path::Path, prefix: &str, into: &mut Vec<String>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &path, into);
            }
            into.push(path);
        }
    }

    /// The index is only sound because an unpacked chain is immutable and fully
    /// materialized: layer application resolves every whiteout by deletion, so a
    /// committed tree carries no markers and its index enumerates it exactly.
    #[test]
    fn a_committed_layer_chain_is_marker_free_and_exactly_enumerated_by_its_index() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let layers = records(1);
        let key = id(&layers);
        let mut draft = snapshots.prepare(Id::new("draft-index").unwrap(), None).unwrap();
        let path = draft.path().to_owned();
        let (ownerships, names) = draft.metadata_mut();
        crate::layer::Layer::new(std::io::Cursor::new(tar(&[
            ("usr/lib/libc.so", b"lower"),
            ("usr/lib/doomed", b"gone"),
            ("etc/hosts", b"hosts"),
        ])))
        .apply_with_metadata(&path, ownerships, names)
        .unwrap();
        let (ownerships, names) = draft.metadata_mut();
        crate::layer::Layer::new(std::io::Cursor::new(tar(&[("usr/lib/.wh.doomed", b"")])))
            .apply_with_metadata(&path, ownerships, names)
            .unwrap();
        draft.commit_layer(key.clone(), layers).unwrap();

        let tree = temp.path().join("committed").join(key.as_str());
        let mut names = Vec::new();
        walk(&tree, "", &mut names);
        names.sort();
        assert!(
            !names.iter().any(|name| name.contains(".wh.")),
            "a committed chain must carry no overlay markers: {names:?}"
        );
        assert!(!names.iter().any(|name| name.ends_with("doomed")), "{names:?}");

        let index = hl_fs::LayerIndex::load(&snapshots.index_path(&key)).unwrap();
        assert!(!index.has_markers());
        assert_eq!(index.len(), names.len());
        for name in &names {
            assert!(index.get(name.as_bytes()).is_some(), "index is missing {name}");
        }
        assert!(index.get(b"usr/lib/doomed").is_none(), "whiteout victim leaked");
        assert!(index.get(b"usr/lib/absent").is_none());
    }

    /// Writable uppers and forked container snapshots publish generically and
    /// must never be indexed: enumerating a mutable tree cannot stay true.
    #[test]
    fn generic_publications_are_never_indexed() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let upper = Id::new("upper-generic").unwrap();
        snapshots
            .prepare(upper.clone(), None)
            .unwrap()
            .commit(upper.clone())
            .unwrap();
        assert!(!snapshots.index_path(&upper).exists());
    }

    #[test]
    fn removing_a_snapshot_reclaims_its_index() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let layers = records(1);
        let key = id(&layers);
        snapshots
            .prepare(Id::new("draft-index-gc").unwrap(), None)
            .unwrap()
            .commit_layer(key.clone(), layers)
            .unwrap();
        assert!(snapshots.index_path(&key).exists());
        assert!(snapshots.remove(&key).unwrap());
        assert!(!snapshots.index_path(&key).exists());
    }

    #[test]
    fn one_two_and_three_layer_publications_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        for count in 1..=3 {
            let layers = records(count);
            let key = id(&layers);
            let snapshots = Snapshots::open(temp.path()).unwrap();
            snapshots
                .prepare(Id::new(format!("draft-{count}")).unwrap(), None)
                .unwrap()
                .commit_layer(key.clone(), layers.clone())
                .unwrap();
            drop(snapshots);

            let reopened = Snapshots::open(temp.path()).unwrap();
            assert_eq!(reopened.layer_records(&key).unwrap(), Some(layers));
            assert!(reopened.view(&key).is_ok());
        }
    }

    #[test]
    fn malformed_and_interrupted_publications_are_not_visible_and_are_reclaimable() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let layers = records(1);
        let key = id(&layers);
        snapshots
            .prepare(Id::new("draft-malformed").unwrap(), None)
            .unwrap()
            .commit_layer(key.clone(), layers)
            .unwrap();
        std::fs::write(snapshots.publication_path(&key), b"not-json").unwrap();

        assert!(!snapshots.contains(&key));
        assert!(snapshots.view(&key).is_err());
        snapshots.discard_unpublished(&key).unwrap();
        assert!(!snapshots.root.join("committed").join(key.as_str()).exists());
        assert!(!snapshots.publication_path(&key).exists());
    }

    #[test]
    fn removal_reclaims_layer_publication_with_snapshot_gc_target() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let layers = records(1);
        let key = id(&layers);
        snapshots
            .prepare(Id::new("draft-gc").unwrap(), None)
            .unwrap()
            .commit_layer(key.clone(), layers)
            .unwrap();

        assert!(snapshots.remove(&key).unwrap());
        assert!(!snapshots.publication_path(&key).exists());
    }

    #[test]
    fn reopen_preserves_locked_live_draft() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let draft = snapshots.prepare(Id::new("live-draft").unwrap(), None).unwrap();

        let reopened = Snapshots::open(temp.path()).unwrap();
        assert!(draft.path().is_dir());
        drop(reopened);
        drop(draft);
    }

    #[test]
    fn reopen_reclaims_unlocked_active_and_unpublished_committed_target() {
        let temp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(temp.path()).unwrap();
        let key = Id::new("abandoned-draft").unwrap();
        let target = Id::new("abandoned-target").unwrap();
        std::fs::create_dir(snapshots.root.join("active").join(key.as_str())).unwrap();
        std::fs::create_dir(snapshots.root.join("committed").join(target.as_str())).unwrap();
        std::fs::write(snapshots.ownership_path("active", &key), b"{}").unwrap();
        std::fs::write(snapshots.names_path("active", &key), b"{}").unwrap();
        std::fs::write(snapshots.ownership_path("committed", &target), b"{}").unwrap();
        std::fs::write(snapshots.names_path("committed", &target), b"{}").unwrap();
        std::fs::write(
            snapshots.root.join("drafts").join(format!("{}.json", key.as_str())),
            serde_json::to_vec(&DraftOwner::publishing(key.clone(), target.clone())).unwrap(),
        )
        .unwrap();
        drop(snapshots);

        let reopened = Snapshots::open(temp.path()).unwrap();
        assert!(!reopened.root.join("active").join(key.as_str()).exists());
        assert!(!reopened.root.join("committed").join(target.as_str()).exists());
    }
}
