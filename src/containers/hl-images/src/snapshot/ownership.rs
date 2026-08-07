use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
};

use crate::{Error, Result, error::At as _};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Ownership {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
struct Entries(BTreeMap<String, Ownership>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Key(String);

impl TryFrom<&Path> for Key {
    type Error = Error;

    fn try_from(path: &Path) -> Result<Self> {
        let lexical = path
            .to_str()
            .ok_or_else(|| Error::InvalidMetadata("ownership path is not UTF-8".into()))?;
        if lexical.is_empty()
            || path.is_absolute()
            || lexical
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(Error::InvalidMetadata(format!(
                "ownership path is not a normalized relative path: {}",
                path.display()
            )));
        }
        Ok(Self(lexical.to_owned()))
    }
}

impl AsRef<str> for Key {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Key> for String {
    fn from(key: Key) -> Self {
        key.0
    }
}

/// Guest ownership metadata for a snapshot.
///
/// Paths are lexical, normalized paths relative to the rootfs. They are never
/// canonicalized, so metadata for a symlink or hard-link name remains attached
/// to that directory entry rather than to its host filesystem target/inode.
#[derive(Clone, Debug)]
pub struct Ownerships {
    path: Option<PathBuf>,
    entries: Entries,
}

impl Ownerships {
    /// Create an in-memory ownership map suitable for composing a new layer.
    #[must_use]
    pub fn memory() -> Self {
        Self {
            path: None,
            entries: Entries::default(),
        }
    }

    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(super) fn create(path: PathBuf) -> Result<Self> {
        let ownerships = Self {
            path: Some(path),
            entries: Entries::default(),
        };
        ownerships.save()?;
        Ok(ownerships)
    }

    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let bytes = fs::read(&path).map_err(|error| Error::LayerFilesystem {
            operation: "read snapshot ownership",
            path: path.clone(),
            source: error,
        })?;
        let entries: Entries = serde_json::from_slice(&bytes).map_err(|error| {
            Error::InvalidMetadata(format!(
                "malformed snapshot ownership sidecar {}: {error}",
                path.display()
            ))
        })?;
        for key in entries.0.keys() {
            Key::try_from(Path::new(key))?;
        }
        Ok(Self {
            path: Some(path),
            entries,
        })
    }

    pub(super) fn fork(source: &Path, path: PathBuf) -> Result<Self> {
        let mut ownerships = Self::open(source.to_owned())?;
        ownerships.path = Some(path);
        ownerships.save()?;
        Ok(ownerships)
    }

    #[must_use]
    pub fn get(&self, path: impl AsRef<Path>) -> Option<Ownership> {
        let key = Key::try_from(path.as_ref()).ok()?;
        self.entries.0.get(key.as_ref()).copied()
    }

    /// Iterate guest ownership entries in deterministic path order.
    pub fn iter(&self) -> impl Iterator<Item = (&Path, Ownership)> {
        self.entries
            .0
            .iter()
            .map(|(path, ownership)| (Path::new(path), *ownership))
    }

    /// Set the guest uid/gid associated with a rootfs directory entry.
    ///
    /// # Errors
    /// Returns an error for a non-normalized or absolute path, or when the
    /// durable sidecar cannot be atomically replaced.
    pub fn set(&mut self, path: impl AsRef<Path>, ownership: Ownership) -> Result<()> {
        let key = String::from(Key::try_from(path.as_ref())?);
        let previous = self.entries.0.insert(key.clone(), ownership);
        if let Err(error) = self.save() {
            match previous {
                Some(value) => self.entries.0.insert(key, value),
                None => self.entries.0.remove(&key),
            };
            return Err(error);
        }
        Ok(())
    }

    /// Remove ownership metadata for a rootfs directory entry.
    ///
    /// # Errors
    /// Returns an error for a non-normalized or absolute path, or when the
    /// durable sidecar cannot be atomically replaced.
    pub fn remove(&mut self, path: impl AsRef<Path>) -> Result<bool> {
        let key = String::from(Key::try_from(path.as_ref())?);
        let Some(previous) = self.entries.0.remove(&key) else {
            return Ok(false);
        };
        if let Err(error) = self.save() {
            self.entries.0.insert(key, previous);
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn record(&mut self, path: &Path, ownership: Ownership) -> Result<()> {
        self.entries.0.insert(String::from(Key::try_from(path)?), ownership);
        Ok(())
    }

    pub(crate) fn discard_tree(&mut self, path: &Path, include_root: bool) -> Result<()> {
        let root = String::from(Key::try_from(path)?);
        let prefix = format!("{root}/");
        self.entries
            .0
            .retain(|entry, _| !(entry.starts_with(&prefix) || (include_root && entry == &root)));
        Ok(())
    }

    pub(crate) fn flush(&self) -> Result<()> {
        self.save()
    }

    /// Set guest ownership for `path` and every descendant without following
    /// symbolic links.
    ///
    /// # Errors
    /// Returns an error for an unsafe relative path, missing filesystem entry,
    /// traversal failure, or sidecar persistence failure.
    pub fn set_recursive(
        &mut self,
        root: impl AsRef<Path>,
        path: impl AsRef<Path>,
        ownership: Ownership,
    ) -> Result<()> {
        let root = root.as_ref();
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            let mut children = fs::read_dir(root)
                .at(root)?
                .collect::<std::io::Result<Vec<_>>>()
                .at(root)?;
            children.sort_by_key(std::fs::DirEntry::file_name);
            for child in children {
                self.record_tree(root, Path::new(&child.file_name()), ownership)?;
            }
        } else {
            Key::try_from(path)?;
            self.record_tree(root, path, ownership)?;
        }
        self.save()
    }

    /// Export `root` as a deterministic archive carrying guest ownership.
    ///
    /// # Errors
    /// Returns an error when the tree cannot be traversed or encoded.
    pub fn archive(&self, root: impl AsRef<Path>, writer: impl Write) -> Result<()> {
        super::archive::write(root.as_ref(), self, writer)
    }

    /// Merge ownership entries below a destination prefix.
    ///
    /// # Errors
    /// Returns an error when the prefix or a resulting path is unsafe.
    pub fn merge(&mut self, prefix: impl AsRef<Path>, source: &Self) -> Result<()> {
        let prefix = prefix.as_ref();
        if !prefix.as_os_str().is_empty() {
            Key::try_from(prefix)?;
        }
        for (path, ownership) in &source.entries.0 {
            let path = if prefix.as_os_str().is_empty() {
                PathBuf::from(path)
            } else {
                prefix.join(path)
            };
            self.record(&path, *ownership)?;
        }
        self.save()
    }

    pub(super) fn relocate(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    fn save(&self) -> Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidMetadata("snapshot ownership sidecar has no parent".into()))?;
        fs::create_dir_all(parent).at(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|name| name.to_str()).unwrap_or("ownership"),
            uuid::Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<()> {
            let mut file = File::create(&temporary).at(&temporary)?;
            serde_json::to_writer(&mut file, &self.entries)?;
            file.write_all(b"\n").at(&temporary)?;
            file.sync_all().at(&temporary)?;
            fs::rename(&temporary, path).at(path)?;
            File::open(parent).at(parent)?.sync_all().at(parent)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn record_tree(&mut self, root: &Path, relative: &Path, ownership: Ownership) -> Result<()> {
        self.record(relative, ownership)?;
        let metadata = fs::symlink_metadata(root.join(relative)).at(root.join(relative))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mut children = fs::read_dir(root.join(relative))
            .at(root.join(relative))?
            .collect::<std::io::Result<Vec<_>>>()
            .at(root.join(relative))?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            self.record_tree(root, &relative.join(child.file_name()), ownership)?;
        }
        Ok(())
    }
}
