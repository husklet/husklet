use std::{
    fs,
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
        let chain = parent.map_or_else(
            || diff.to_string(),
            |parent| format!("{} {diff}", parent.as_str()),
        );
        Self::new(format!(
            "chain-{}",
            Digest::sha256(chain.as_bytes()).encoded()
        ))
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
        fs::create_dir_all(root.as_ref().join("work"))?;
        Ok(Self {
            root: root.as_ref().to_owned(),
        })
    }
    /// # Errors
    /// Returns an error for a duplicate key, missing parent, or copy failure.
    pub fn prepare(&self, key: Id, parent: Option<&Id>) -> Result<Draft> {
        let path = self.root.join("active").join(key.as_str());
        fs::create_dir(&path)?;
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
        })
    }
    /// # Errors
    /// Returns an error when the committed snapshot does not exist.
    pub fn view(&self, id: &Id) -> Result<View> {
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
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(self.ownership_path("committed", id));
                let _ = fs::remove_file(self.names_path("committed", id));
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
}

pub struct Draft {
    path: PathBuf,
    key: Id,
    root: PathBuf,
    ownership: Ownerships,
    names: Names,
    finished: bool,
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
    pub fn commit(mut self, id: Id) -> Result<View> {
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
        }
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
