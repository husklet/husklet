use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{Error, Result};

pub(super) struct Tree<'a>(&'a Path);

struct Entry(fs::DirEntry);

impl<'a> From<&'a Path> for Tree<'a> {
    fn from(path: &'a Path) -> Self {
        Self(path)
    }
}

impl Tree<'_> {
    pub(super) fn writable(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(self.0) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(permissions.mode() | 0o700);
            fs::set_permissions(self.0, permissions)?;
        }
        #[cfg(not(unix))]
        if permissions.readonly() {
            permissions.set_readonly(false);
            fs::set_permissions(self.0, permissions)?;
        }
        for entry in fs::read_dir(self.0)? {
            let path = entry?.path();
            Tree::from(path.as_path()).writable()?;
        }
        Ok(())
    }

    pub(super) fn copy_to(&self, target: &Path) -> Result<()> {
        self.copy(target, &mut BTreeMap::new())
    }

    fn copy(&self, target: &Path, links: &mut BTreeMap<(u64, u64), PathBuf>) -> Result<()> {
        let source = self.0;
        if !source.is_dir() {
            return Err(Error::InvalidMetadata(
                "parent snapshot does not exist".into(),
            ));
        }
        let original = fs::symlink_metadata(source)?.permissions();
        let mut accessible = original.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            accessible.set_mode(accessible.mode() | 0o700);
        }
        #[cfg(not(unix))]
        accessible.set_readonly(false);
        fs::set_permissions(source, accessible).map_err(|error| Error::LayerFilesystem {
            operation: "make snapshot directory traversable",
            path: source.to_owned(),
            source: error,
        })?;
        let copied = (|| -> Result<()> {
            let entries = fs::read_dir(source).map_err(|source_error| Error::LayerFilesystem {
                operation: "read snapshot directory",
                path: source.to_owned(),
                source: source_error,
            })?;
            for item in entries {
                let item = item.map_err(|source_error| Error::LayerFilesystem {
                    operation: "read snapshot entry",
                    path: source.to_owned(),
                    source: source_error,
                })?;
                Entry(item).copy(target, links)?;
            }
            Ok(())
        })();
        let restored =
            fs::set_permissions(source, original).map_err(|error| Error::LayerFilesystem {
                operation: "restore snapshot directory permissions",
                path: source.to_owned(),
                source: error,
            });
        copied?;
        restored?;
        Ok(())
    }

    fn copy_file(
        source: &Path,
        destination: &Path,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> Result<()> {
        let original = fs::metadata(source)?.permissions();
        let mut readable = original.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            readable.set_mode(readable.mode() | 0o400);
        }
        #[cfg(not(unix))]
        readable.set_readonly(false);
        fs::set_permissions(source, readable).map_err(|error| Error::LayerFilesystem {
            operation: "make snapshot file readable",
            path: source.to_owned(),
            source: error,
        })?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = fs::metadata(source)?;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(unix)]
        let copied = if let Some(first) = links.get(&identity) {
            fs::hard_link(first, destination)
        } else {
            reflink_copy::reflink_or_copy(source, destination).map(|_| {
                links.insert(identity, destination.to_owned());
            })
        };
        #[cfg(not(unix))]
        let copied = reflink_copy::reflink_or_copy(source, destination).map(|_| ());
        let restored = fs::set_permissions(source, original.clone());
        copied.map_err(|error| Error::LayerFilesystem {
            operation: "copy snapshot file",
            path: source.to_owned(),
            source: error,
        })?;
        restored.map_err(|error| Error::LayerFilesystem {
            operation: "restore snapshot file permissions",
            path: source.to_owned(),
            source: error,
        })?;
        fs::set_permissions(destination, original).map_err(|error| Error::LayerFilesystem {
            operation: "preserve snapshot file permissions",
            path: destination.to_owned(),
            source: error,
        })?;
        Ok(())
    }
}

impl Entry {
    fn copy(&self, target: &Path, links: &mut BTreeMap<(u64, u64), PathBuf>) -> Result<()> {
        let meta = self
            .0
            .file_type()
            .map_err(|source| Error::LayerFilesystem {
                operation: "inspect snapshot entry",
                path: self.0.path(),
                source,
            })?;
        let destination = target.join(self.0.file_name());
        if meta.is_dir() {
            return self.copy_tree(&destination, links);
        }
        if meta.is_file() {
            return Tree::copy_file(&self.0.path(), &destination, links);
        }
        if meta.is_symlink() {
            return self.copy_link(&destination);
        }
        Err(Error::InvalidMetadata(format!(
            "special snapshot node is unsupported: {}",
            self.0.path().display()
        )))
    }

    fn copy_tree(
        &self,
        destination: &Path,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> Result<()> {
        fs::create_dir(destination).map_err(|source| Error::LayerFilesystem {
            operation: "create snapshot directory",
            path: self.0.path(),
            source,
        })?;
        let path = self.0.path();
        Tree::from(path.as_path()).copy(destination, links)?;
        let permissions = fs::metadata(&path)
            .map_err(|source| Error::LayerFilesystem {
                operation: "read snapshot directory permissions",
                path: path.clone(),
                source,
            })?
            .permissions();
        fs::set_permissions(destination, permissions).map_err(|source| Error::LayerFilesystem {
            operation: "set snapshot directory permissions",
            path,
            source,
        })?;
        Ok(())
    }

    #[cfg(unix)]
    fn copy_link(&self, destination: &Path) -> Result<()> {
        let target = fs::read_link(self.0.path()).map_err(|source| Error::LayerFilesystem {
            operation: "read snapshot symbolic link",
            path: self.0.path(),
            source,
        })?;
        std::os::unix::fs::symlink(target, destination).map_err(|source| {
            Error::LayerFilesystem {
                operation: "copy snapshot symbolic link",
                path: self.0.path(),
                source,
            }
        })?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn copy_link(&self, _destination: &Path) -> Result<()> {
        Err(Error::InvalidMetadata(
            "symlink snapshots unsupported".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::Tree;
    use std::fs;

    #[test]
    fn copies_nested_files_and_preserves_source_contents() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/value"), b"snapshot").unwrap();

        Tree::from(source.path()).copy_to(target.path()).unwrap();

        assert_eq!(
            fs::read(target.path().join("nested/value")).unwrap(),
            b"snapshot"
        );
        assert_eq!(
            fs::read(source.path().join("nested/value")).unwrap(),
            b"snapshot"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_hard_links_and_symbolic_links() {
        use std::os::unix::fs::{symlink, MetadataExt as _};

        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(source.path().join("first"), b"linked").unwrap();
        fs::hard_link(source.path().join("first"), source.path().join("second")).unwrap();
        symlink("first", source.path().join("symbolic")).unwrap();

        Tree::from(source.path()).copy_to(target.path()).unwrap();

        let first = fs::metadata(target.path().join("first")).unwrap();
        let second = fs::metadata(target.path().join("second")).unwrap();
        assert_eq!(first.ino(), second.ino());
        assert_eq!(
            fs::read_link(target.path().join("symbolic")).unwrap(),
            std::path::PathBuf::from("first")
        );
    }

    #[cfg(unix)]
    #[test]
    fn writable_recurses_through_read_only_directories() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o500)).unwrap();

        Tree::from(root.path()).writable().unwrap();

        assert_ne!(
            fs::metadata(nested).unwrap().permissions().mode() & 0o200,
            0
        );
    }
}
