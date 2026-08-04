use crate::Result;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path as FsPath, PathBuf};

/// Runtime-neutral classification of a rootfs change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
}

/// One path whose current state differs from its immutable image baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// Deterministic rootfs comparison result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Changes(Vec<Change>);

impl From<Vec<Change>> for Changes {
    fn from(value: Vec<Change>) -> Self {
        Self(value)
    }
}

impl Changes {
    /// Compares two rootfs trees without following symlinks.
    ///
    /// # Errors
    /// Returns filesystem errors or non-UTF-8 path failures.
    pub fn between(baseline: impl AsRef<FsPath>, current: impl AsRef<FsPath>) -> Result<Self> {
        let before = Inventory::read(baseline.as_ref())?;
        let after = Inventory::read(current.as_ref())?;
        let mut paths = before
            .entries
            .keys()
            .chain(after.entries.keys())
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        let changes = paths
            .into_iter()
            .filter_map(|path| {
                let kind = match (before.entries.get(&path), after.entries.get(&path)) {
                    (None, Some(_)) => Some(ChangeKind::Added),
                    (Some(_), None) => Some(ChangeKind::Deleted),
                    (Some(left), Some(right)) if !left.same(right, &before.root, &after.root, &path) => {
                        Some(ChangeKind::Modified)
                    }
                    _ => None,
                }?;
                Some(Change {
                    path: FsPath::new("/").join(path),
                    kind,
                })
            })
            .collect();
        Ok(Self(changes))
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<Change> {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    kind: u8,
    mode: u32,
    size: u64,
    link: Option<OsString>,
}

struct Inventory {
    root: PathBuf,
    entries: BTreeMap<PathBuf, Entry>,
}

impl Inventory {
    fn read(root: &FsPath) -> Result<Self> {
        let mut entries = BTreeMap::new();
        Self::walk(root, FsPath::new(""), &mut entries)?;
        Ok(Self {
            root: root.to_owned(),
            entries,
        })
    }

    fn walk(root: &FsPath, relative: &FsPath, entries: &mut BTreeMap<PathBuf, Entry>) -> Result<()> {
        let mut children = fs::read_dir(root.join(relative))?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = relative.join(child.file_name());
            let metadata = fs::symlink_metadata(child.path())?;
            #[cfg(unix)]
            let mode = metadata.permissions().mode();
            #[cfg(not(unix))]
            let mode = 0;
            let kind = if metadata.is_file() {
                1
            } else if metadata.is_dir() {
                2
            } else if metadata.file_type().is_symlink() {
                3
            } else {
                4
            };
            entries.insert(
                path.clone(),
                Entry {
                    kind,
                    mode,
                    size: if kind == 1 { metadata.len() } else { 0 },
                    link: (kind == 3)
                        .then(|| fs::read_link(child.path()).map(PathBuf::into_os_string))
                        .transpose()?,
                },
            );
            if kind == 2 {
                Self::walk(root, &path, entries)?;
            }
        }
        Ok(())
    }
}

impl Entry {
    fn same(&self, other: &Self, left: &FsPath, right: &FsPath, path: &FsPath) -> bool {
        if self != other {
            return false;
        }
        if self.kind != 1 {
            return true;
        }
        match (fs::read(left.join(path)), fs::read(right.join(path))) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}
