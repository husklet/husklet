use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    Error, Result,
    rootfs::{Change, ChangeKind},
    snapshot::View as SnapshotView,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    kind: u8,
    mode: u32,
    size: u64,
    link: Option<OsString>,
    ownership: Option<crate::snapshot::Ownership>,
    source: PathBuf,
}

pub(super) struct Tree {
    entries: BTreeMap<PathBuf, Entry>,
}

impl Tree {
    pub(super) fn read(view: &SnapshotView) -> Result<Self> {
        let mut tree = Self {
            entries: BTreeMap::new(),
        };
        tree.walk(view, Path::new(""))?;
        Ok(tree)
    }

    pub(super) fn merge(lower: &SnapshotView, upper: &SnapshotView) -> Result<Self> {
        let mut tree = Self::read(lower)?;
        let mut upper_entries = BTreeMap::new();
        let mut whiteouts = BTreeSet::new();
        let mut opaque = BTreeSet::new();
        Self::walk_overlay(upper, Path::new(""), &mut upper_entries, &mut whiteouts, &mut opaque)?;
        for directory in opaque {
            let prefix = directory.join("");
            tree.entries
                .retain(|path, _| path == &directory || !path.starts_with(&prefix));
        }
        for victim in whiteouts {
            tree.entries
                .retain(|path, _| path != &victim && !path.starts_with(&victim));
        }
        for (path, mut entry) in upper_entries {
            if entry.ownership.is_none() {
                entry.ownership = tree.entries.get(&path).and_then(|lower| lower.ownership);
            }
            tree.entries.insert(path, entry);
        }
        Ok(tree)
    }

    fn walk(&mut self, view: &SnapshotView, physical: &Path) -> Result<()> {
        let mut children = fs::read_dir(view.path().join(physical))?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let physical_path = physical.join(child.file_name());
            let guest = view.names().guest(&physical_path).to_owned();
            if guest
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".wh."))
            {
                continue;
            }
            let entry = Entry::read(view, &physical_path, &guest)?;
            let directory = entry.kind == 2;
            self.entries.insert(guest, entry);
            if directory {
                self.walk(view, &physical_path)?;
            }
        }
        Ok(())
    }

    fn whiteout_target(name: &str) -> Result<&str> {
        if name.is_empty() {
            return Err(Error::InvalidMetadata("empty overlay whiteout target".into()));
        }
        Ok(name)
    }

    fn walk_overlay(
        view: &SnapshotView,
        physical: &Path,
        entries: &mut BTreeMap<PathBuf, Entry>,
        whiteouts: &mut BTreeSet<PathBuf>,
        opaque: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        let mut children = fs::read_dir(view.path().join(physical))?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let physical_path = physical.join(child.file_name());
            let guest = view.names().guest(&physical_path).to_owned();
            let name = guest
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::InvalidMetadata(format!("rootfs path is not UTF-8: {}", guest.display())))?;
            if name == ".wh..wh..opq" {
                opaque.insert(guest.parent().unwrap_or(Path::new("")).to_owned());
                continue;
            }
            if let Some(name) = name.strip_prefix(".wh.") {
                let name = Self::whiteout_target(name)?;
                whiteouts.insert(guest.parent().unwrap_or(Path::new("")).join(name));
                continue;
            }
            let entry = Entry::read(view, &physical_path, &guest)?;
            let directory = entry.kind == 2;
            entries.insert(guest, entry);
            if directory {
                Self::walk_overlay(view, &physical_path, entries, whiteouts, opaque)?;
            }
        }
        Ok(())
    }
}

impl Entry {
    fn read(view: &SnapshotView, physical: &Path, guest: &Path) -> Result<Self> {
        let source = view.path().join(physical);
        let metadata = fs::symlink_metadata(&source)?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode()
        };
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
        Ok(Self {
            kind,
            mode,
            size: if kind == 1 { metadata.len() } else { 0 },
            link: (kind == 3)
                .then(|| fs::read_link(&source).map(PathBuf::into_os_string))
                .transpose()?,
            ownership: view.ownership().get(guest),
            source,
        })
    }

    fn same(&self, other: &Self) -> Result<bool> {
        if self.kind != other.kind
            || self.mode != other.mode
            || self.size != other.size
            || self.link != other.link
            || self.ownership != other.ownership
        {
            return Ok(false);
        }
        Ok(self.kind != 1 || self.source == other.source || fs::read(&self.source)? == fs::read(&other.source)?)
    }
}

pub(super) struct Changes(Vec<Change>);

impl Changes {
    pub(super) fn between(before: &Tree, after: &Tree) -> Result<Self> {
        let paths = before
            .entries
            .keys()
            .chain(after.entries.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut changes = Vec::new();
        for path in paths {
            let kind = match (before.entries.get(&path), after.entries.get(&path)) {
                (None, Some(_)) => Some(ChangeKind::Added),
                (Some(_), None) => Some(ChangeKind::Deleted),
                (Some(left), Some(right)) if !left.same(right)? => Some(ChangeKind::Modified),
                _ => None,
            };
            if let Some(kind) = kind {
                changes.push(Change {
                    path: Path::new("/").join(path),
                    kind,
                });
            }
        }
        Ok(Self(changes))
    }

    pub(super) fn into_inner(self) -> Vec<Change> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Leases,
        rootfs::Roots,
        snapshot::{Id, Snapshots},
    };

    #[test]
    fn overlay_changes_merge_add_modify_whiteout_and_opaque_directory() {
        let root = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::open(root.path().join("snapshots")).unwrap();
        let lower = Id::new("lower-diff").unwrap();
        let mut draft = snapshots.prepare(lower.clone(), None).unwrap();
        std::fs::create_dir_all(draft.path().join("opaque/nested")).unwrap();
        std::fs::write(draft.path().join("keep"), b"same").unwrap();
        std::fs::write(draft.path().join("modify"), b"before").unwrap();
        std::fs::write(draft.path().join("delete"), b"gone").unwrap();
        std::fs::write(draft.path().join("owner"), b"same").unwrap();
        std::os::unix::fs::symlink("keep", draft.path().join("link")).unwrap();
        std::fs::write(draft.path().join("opaque/gone"), b"gone").unwrap();
        std::fs::write(draft.path().join("opaque/nested/value"), b"gone").unwrap();
        draft
            .ownership_mut()
            .set("owner", crate::snapshot::Ownership { uid: 1, gid: 1 })
            .unwrap();
        draft.commit(lower.clone()).unwrap();
        let roots = Roots::new(snapshots, Leases::open(root.path().join("metadata")).unwrap());
        let reference = roots.fork_overlay(&lower).unwrap();
        let overlay = roots.open_overlay(&reference).unwrap();
        std::fs::write(overlay.upper().join("modify"), b"after").unwrap();
        std::fs::write(overlay.upper().join("added"), b"new").unwrap();
        std::fs::write(overlay.upper().join("owner"), b"same").unwrap();
        std::os::unix::fs::symlink("modify", overlay.upper().join("link")).unwrap();
        std::fs::write(overlay.upper().join(".wh.delete"), b"").unwrap();
        std::fs::create_dir_all(overlay.upper().join("opaque")).unwrap();
        std::fs::write(overlay.upper().join("opaque/.wh..wh..opq"), b"").unwrap();
        std::fs::write(overlay.upper().join("opaque/new"), b"new").unwrap();
        std::fs::write(
            root.path()
                .join("snapshots/ownership/committed")
                .join(format!("{}.json", reference.overlay().unwrap().upper().as_str())),
            br#"{"owner":{"uid":2,"gid":2}}"#,
        )
        .unwrap();

        assert_eq!(
            roots.changes(&reference).unwrap(),
            vec![
                Change {
                    path: "/added".into(),
                    kind: ChangeKind::Added,
                },
                Change {
                    path: "/delete".into(),
                    kind: ChangeKind::Deleted,
                },
                Change {
                    path: "/link".into(),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: "/modify".into(),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: "/opaque/gone".into(),
                    kind: ChangeKind::Deleted,
                },
                Change {
                    path: "/opaque/nested".into(),
                    kind: ChangeKind::Deleted,
                },
                Change {
                    path: "/opaque/nested/value".into(),
                    kind: ChangeKind::Deleted,
                },
                Change {
                    path: "/opaque/new".into(),
                    kind: ChangeKind::Added,
                },
                Change {
                    path: "/owner".into(),
                    kind: ChangeKind::Modified,
                },
            ]
        );
        assert_eq!(std::fs::read(overlay.lower().join("modify")).unwrap(), b"before");
        assert!(overlay.lower().join("delete").exists());

        let overlay = roots.open_overlay(&reference).unwrap();
        let mut bytes = Vec::new();
        overlay.archive_upper(&mut bytes).unwrap();
        let mut entries = tar::Archive::new(bytes.as_slice())
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.path().unwrap().into_owned(),
                    entry.header().uid().unwrap(),
                    entry.header().gid().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        assert!(entries.iter().any(|entry| entry.0 == Path::new(".wh.delete")));
        assert!(entries.iter().any(|entry| entry.0 == Path::new("opaque/.wh..wh..opq")));
        assert!(
            entries
                .iter()
                .any(|entry| entry.0 == Path::new("owner") && entry.1 == 2 && entry.2 == 2)
        );
        assert!(!entries.iter().any(|entry| entry.0 == Path::new("keep")));
    }
}
