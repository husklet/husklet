use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{Digest, Error, Result};

mod ownership;
pub use ownership::{Ownership, Ownerships};
mod names;
pub use names::Names;
mod archive;
mod tree;
use tree::Tree;
mod record;
pub(crate) use record::LayerRecord;
use record::{DraftOwner, Publication};
use crate::storage::{Native, Persistence as _};
use fs2::FileExt as _;

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Id(String);
impl Id {
    /// # Errors
    /// Returns an error when the identifier is empty or filesystem-unsafe.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err(Error::InvalidMetadata("invalid snapshot id".into()));
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn chain(parent: Option<&Self>, diff: &Digest) -> Result<Self> {
        let chain = match parent {
            None => diff.clone(),
            Some(parent) => {
                let encoded = parent.as_str().strip_prefix("chain-").ok_or_else(|| {
                    Error::InvalidMetadata(format!(
                        "snapshot {} is not a layer chain",
                        parent.as_str()
                    ))
                })?;
                let parent: Digest = format!("sha256:{encoded}").parse()?;
                Digest::sha256(format!("{parent} {diff}").as_bytes())
            }
        };
        Self::new(format!("chain-{}", chain.encoded()))
    }

    pub(crate) fn chain_digest(&self) -> Result<Digest> {
        let encoded = self
            .as_str()
            .strip_prefix("chain-")
            .ok_or_else(|| Error::InvalidMetadata(format!("snapshot {} is not a layer chain", self.as_str())))?;
        format!("sha256:{encoded}").parse()
    }
}

#[cfg(test)]
mod id_tests {
    use super::Id;
    use crate::Digest;

    #[test]
    fn layer_chain_matches_oci_identity_chain_id() {
        let first_diff = Digest::sha256(b"first layer tar");
        let second_diff = Digest::sha256(b"second layer tar");

        let first = Id::chain(None, &first_diff).unwrap();
        assert_eq!(first.as_str(), format!("chain-{}", first_diff.encoded()));

        let second = Id::chain(Some(&first), &second_diff).unwrap();
        let expected = Digest::sha256(format!("{first_diff} {second_diff}").as_bytes());
        assert_eq!(second.as_str(), format!("chain-{}", expected.encoded()));
    }

    #[test]
    fn layer_chain_rejects_non_chain_parent() {
        let parent = Id::new("container-root").unwrap();
        let diff = Digest::sha256(b"layer tar");

        assert!(Id::chain(Some(&parent), &diff).is_err());
    }
}

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
        fs::create_dir_all(root.as_ref().join("active"))?;
        fs::create_dir_all(root.as_ref().join("committed"))?;
        fs::create_dir_all(root.as_ref().join("ownership/active"))?;
        fs::create_dir_all(root.as_ref().join("ownership/committed"))?;
        fs::create_dir_all(root.as_ref().join("names/active"))?;
        fs::create_dir_all(root.as_ref().join("names/committed"))?;
        fs::create_dir_all(root.as_ref().join("publication/committed"))?;
        fs::create_dir_all(root.as_ref().join("drafts"))?;
        fs::create_dir_all(root.as_ref().join("draft-locks"))?;
        fs::create_dir_all(root.as_ref().join("work"))?;
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
            .open(lock_path)?;
        lock.lock_exclusive()?;
        self.cleanup_active(&key)?;
        Native.replace(
            &self.root.join("drafts").join(format!("{}.json", key.as_str())),
            &serde_json::to_vec(&DraftOwner::active(key.clone()))?,
        )?;
        let path = self.root.join("active").join(key.as_str());
        if let Err(error) = fs::create_dir(&path) {
            let _ = Native.remove(&self.root.join("drafts").join(format!("{}.json", key.as_str())));
            return Err(error.into());
        }
        let ownership_path = self.ownership_path("active", &key);
        let names_path = self.names_path("active", &key);
        let ownership = match parent {
            Some(parent) => {
                let parent_path = self.root.join("committed").join(parent.as_str());
                let result = Tree::from(parent_path.as_path())
                    .copy_to(&path)
                    .and_then(|()| {
                        Ownerships::fork(
                            &self.ownership_path("committed", parent),
                            ownership_path.clone(),
                        )
                    })
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
            None => match Ownerships::create(ownership_path.clone()).and_then(|ownership| {
                Names::create(names_path.clone()).map(|names| (ownership, names))
            }) {
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
            return Err(Error::InvalidMetadata(format!(
                "unknown snapshot {}",
                id.as_str()
            )));
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
        Ok(fs::read_dir(path)?.next().transpose()?.is_none())
    }

    /// List committed snapshots in deterministic identifier order.
    ///
    /// # Errors
    /// Returns an error when the snapshot directory is unreadable or malformed.
    pub fn committed(&self) -> Result<Vec<Id>> {
        let mut snapshots = Vec::new();
        for entry in fs::read_dir(self.root.join("committed"))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
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
        match fs::remove_dir_all(path) {
            Ok(()) => {
                match fs::remove_file(self.ownership_path("committed", id)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                let _ = fs::remove_file(self.names_path("committed", id));
                let _ = Native.remove(&self.publication_path(id));
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(self.ownership_path("committed", id));
                let _ = fs::remove_file(self.names_path("committed", id));
                let _ = Native.remove(&self.publication_path(id));
                Ok(false)
            }
            Err(error) => Err(error.into()),
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

    fn publication_path(&self, id: &Id) -> PathBuf {
        self.root
            .join("publication/committed")
            .join(format!("{}.json", id.as_str()))
    }

    fn publication(&self, id: &Id) -> Result<Publication> {
        let path = self.publication_path(id);
        let file = File::open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::InvalidMetadata(format!("snapshot {} is not completely published", id.as_str()))
            } else {
                error.into()
            }
        })?;
        const MAX_PUBLICATION_BYTES: u64 = 128 * 1024;
        let mut bytes = Vec::new();
        file.take(MAX_PUBLICATION_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PUBLICATION_BYTES {
            return Err(Error::InvalidMetadata("snapshot publication exceeds 128 KiB".into()));
        }
        let publication = serde_json::from_slice::<Publication>(&bytes)
            .map_err(|error| Error::InvalidMetadata(format!("malformed snapshot publication {}: {error}", path.display())))?
            .validate()?;
        publication.validate_key(id)?;
        Ok(publication)
    }

    fn recover_abandoned_drafts(&self) -> Result<()> {
        for entry in fs::read_dir(self.root.join("drafts"))? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let key = Id::new(name)?;
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(self.root.join("draft-locks").join(format!("{name}.lock")))?;
            match lock.try_lock_exclusive() {
                Ok(()) => {
                    let owner_path = self.root.join("drafts").join(format!("{name}.json"));
                    let bytes = fs::read(&owner_path)?;
                    if bytes.len() > 4096 {
                        return Err(Error::InvalidMetadata("snapshot draft owner exceeds 4 KiB".into()));
                    }
                    let target = serde_json::from_slice::<DraftOwner>(&bytes)
                        .map_err(|error| Error::InvalidMetadata(format!("malformed snapshot draft owner: {error}")))?
                        .validate(&key)?;
                    self.cleanup_active(&key)?;
                    if let Some(target) = target
                        && !self.contains(&target)
                    {
                        self.remove(&target)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn cleanup_active(&self, key: &Id) -> Result<()> {
        let path = self.root.join("active").join(key.as_str());
        Tree::from(path.as_path()).writable()?;
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for path in [self.ownership_path("active", key), self.names_path("active", key)] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let _ = Native.remove(&self.root.join("drafts").join(format!("{}.json", key.as_str())))?;
        Ok(())
    }
}

pub struct Draft {
    path: PathBuf,
    key: Id,
    root: PathBuf,
    ownership: Ownerships,
    names: Names,
    finished: bool,
    lock: File,
}
impl Draft {
    #[must_use]
    pub fn key(&self) -> &Id {
        &self.key
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[must_use]
    pub fn ownership(&self) -> &Ownerships {
        &self.ownership
    }
    /// Mutably access durable guest ownership metadata for this draft.
    ///
    /// Changes made through [`Ownerships::set`] and [`Ownerships::remove`] are
    /// persisted atomically before those methods return.
    pub fn ownership_mut(&mut self) -> &mut Ownerships {
        &mut self.ownership
    }
    #[must_use]
    pub fn names(&self) -> &Names {
        &self.names
    }
    pub fn names_mut(&mut self) -> &mut Names {
        &mut self.names
    }
    /// Mutably borrow both transactional metadata maps for a layer operation.
    pub fn metadata_mut(&mut self) -> (&mut Ownerships, &mut Names) {
        (&mut self.ownership, &mut self.names)
    }
    /// # Errors
    /// Returns an error when the snapshot cannot be atomically committed and synchronized.
    pub fn commit(self, id: Id) -> Result<View> {
        self.commit_with(id, Publication::generic())
    }

    pub(crate) fn commit_layer(self, id: Id, layers: Vec<LayerRecord>) -> Result<View> {
        let publication = Publication::layer_chain(layers)?;
        self.commit_with(id, publication)
    }

    fn commit_with(mut self, id: Id, publication: Publication) -> Result<View> {
        publication.validate_key(&id)?;
        Native.replace(
            &self.root.join("drafts").join(format!("{}.json", self.key.as_str())),
            &serde_json::to_vec(&DraftOwner::publishing(self.key.clone(), id.clone()))?,
        )?;
        Tree::from(self.path.as_path()).sync()?;
        let target = self.root.join("committed").join(id.as_str());
        let ownership_target = self
            .root
            .join("ownership/committed")
            .join(format!("{}.json", id.as_str()));
        let names_target = self
            .root
            .join("names/committed")
            .join(format!("{}.json", id.as_str()));
        fs::rename(&self.path, &target)?;
        let ownership_path = self
            .ownership
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft ownership sidecar is absent".into()))?;
        if let Err(error) = fs::rename(ownership_path, &ownership_target) {
            let _ = fs::rename(&target, &self.path);
            return Err(error.into());
        }
        let names_path = self
            .names
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft names sidecar is absent".into()))?;
        if let Err(error) = fs::rename(names_path, &names_target) {
            let _ = fs::rename(
                &ownership_target,
                self.root
                    .join("ownership/active")
                    .join(format!("{}.json", self.key.as_str())),
            );
            let _ = fs::rename(&target, &self.path);
            return Err(error.into());
        }
        self.ownership.relocate(ownership_target);
        self.names.relocate(names_target);
        self.finished = true;
        Self::sync(&target)?;
        File::open(self.root.join("ownership/committed"))?.sync_all()?;
        File::open(self.root.join("names/committed"))?.sync_all()?;
        Native.replace(
            &self
                .root
                .join("publication/committed")
                .join(format!("{}.json", id.as_str())),
            &serde_json::to_vec(&publication)?,
        )?;
        Native.remove(&self.root.join("drafts").join(format!("{}.json", self.key.as_str())))?;
        Ok(View {
            id,
            path: Arc::new(target),
            ownership: self.ownership.clone(),
            names: self.names.clone(),
        })
    }
    /// # Errors
    /// Returns an error when the active snapshot cannot be removed.
    pub fn abort(mut self) -> Result<()> {
        Tree::from(self.path.as_path()).writable()?;
        fs::remove_dir_all(&self.path)?;
        let ownership_path = self
            .ownership
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft ownership sidecar is absent".into()))?;
        fs::remove_file(ownership_path)?;
        let names_path = self
            .names
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft names sidecar is absent".into()))?;
        fs::remove_file(names_path)?;
        self.finished = true;
        Native.remove(&self.root.join("drafts").join(format!("{}.json", self.key.as_str())))?;
        Ok(())
    }

    fn sync(path: &Path) -> Result<()> {
        std::fs::File::open(path)?.sync_all()?;
        std::fs::File::open(path.parent().expect("snapshot parent"))?.sync_all()?;
        Ok(())
    }
}
impl Drop for Draft {
    fn drop(&mut self) {
        if !self.finished {
            let _ = Tree::from(self.path.as_path()).writable();
            let _ = fs::remove_dir_all(&self.path);
            if let Some(path) = self.ownership.path() {
                let _ = fs::remove_file(path);
            }
            if let Some(path) = self.names.path() {
                let _ = fs::remove_file(path);
            }
            let _ = Native.remove(&self.root.join("drafts").join(format!("{}.json", self.key.as_str())));
        }
        let _ = self.lock.unlock();
    }
}

#[derive(Clone, Debug)]
pub struct View {
    id: Id,
    path: Arc<PathBuf>,
    ownership: Ownerships,
    names: Names,
}
impl View {
    #[must_use]
    pub fn id(&self) -> &Id {
        &self.id
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[must_use]
    pub fn ownership(&self) -> &Ownerships {
        &self.ownership
    }
    #[must_use]
    pub fn names(&self) -> &Names {
        &self.names
    }

    /// Export this snapshot as a deterministic tar stream using guest uid/gid
    /// metadata rather than host filesystem ownership.
    ///
    /// # Errors
    /// Returns an error when the snapshot cannot be traversed or encoded.
    pub fn archive(&self, writer: impl std::io::Write) -> Result<()> {
        archive::write_names(&self.path, &self.ownership, Some(&self.names), writer)
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
                LayerRecord::new(diff_id, parent.clone(), chain_id.clone(), DiffSize::new(index as u64 + 1))
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
            .commit_layer(key.clone(), layers, )
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
