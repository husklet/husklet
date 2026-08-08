//! In-progress snapshot drafts and their committed read-only views.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{Error, Result, error::At as _};

use crate::storage::{Native, Persistence as _};

use super::{DraftOwner, Id, LayerRecord, Names, Ownerships, Publication, Tree, archive};

pub struct Draft {
    pub(super) path: PathBuf,
    pub(super) key: Id,
    pub(super) root: PathBuf,
    pub(super) ownership: Ownerships,
    pub(super) names: Names,
    pub(super) finished: bool,
    pub(super) lock: File,
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

    // Consumes the publication it commits, so a draft cannot be published twice.
    #[allow(clippy::needless_pass_by_value)]
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
        let names_target = self.root.join("names/committed").join(format!("{}.json", id.as_str()));
        fs::rename(&self.path, &target).at(&target)?;
        let ownership_path = self
            .ownership
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft ownership sidecar is absent".into()))?;
        if let Err(error) = fs::rename(ownership_path, &ownership_target) {
            let _ = fs::rename(&target, &self.path);
            return Err(error).at(ownership_target);
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
            return Err(error).at(names_target);
        }
        self.ownership.relocate(ownership_target);
        self.names.relocate(names_target);
        self.finished = true;
        Self::sync(&target)?;
        for directory in [self.root.join("ownership/committed"), self.root.join("names/committed")] {
            File::open(&directory).at(&directory)?.sync_all().at(&directory)?;
        }
        // The tree is final and immutable from here, so its name index is emitted
        // before the publication record that makes the snapshot visible: nothing
        // can observe a published chain whose index is still being written.
        if publication.is_layer_chain() {
            super::index::publish(&self.root, &id, &target)?;
        }
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
        fs::remove_dir_all(&self.path).at(&self.path)?;
        let ownership_path = self
            .ownership
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft ownership sidecar is absent".into()))?;
        fs::remove_file(ownership_path).at(ownership_path)?;
        let names_path = self
            .names
            .path()
            .ok_or_else(|| Error::InvalidMetadata("draft names sidecar is absent".into()))?;
        fs::remove_file(names_path).at(names_path)?;
        self.finished = true;
        Native.remove(&self.root.join("drafts").join(format!("{}.json", self.key.as_str())))?;
        Ok(())
    }

    fn sync(path: &Path) -> Result<()> {
        let parent = path.parent().expect("snapshot parent");
        File::open(path).at(path)?.sync_all().at(path)?;
        File::open(parent).at(parent)?.sync_all().at(parent)?;
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
    pub(super) id: Id,
    pub(super) path: Arc<PathBuf>,
    pub(super) ownership: Ownerships,
    pub(super) names: Names,
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
