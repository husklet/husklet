use crate::{Error, Result};
use std::fs;
use std::path::{Component, Path as FsPath, PathBuf};

/// Validated guest or archive-entry path within a container filesystem operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path(PathBuf);

impl Path {
    pub(super) fn guest(path: &FsPath) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::InvalidSpec("container path must be absolute".into()));
        }
        let mut clean = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(value) => clean.push(value),
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(Error::InvalidSpec(
                        "container path contains traversal".into(),
                    ));
                }
            }
        }
        Ok(Self(clean))
    }

    pub(super) fn entry(path: &FsPath) -> Result<Self> {
        let mut clean = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => clean.push(value),
                Component::CurDir => {}
                Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                    return Err(Error::InvalidSpec(
                        "archive entry contains traversal".into(),
                    ));
                }
            }
        }
        if clean.as_os_str().is_empty() {
            return Err(Error::InvalidSpec("archive entry path is empty".into()));
        }
        Ok(Self(clean))
    }

    pub(super) fn as_path(&self) -> &FsPath {
        &self.0
    }

    pub(super) fn output(&self, root: &FsPath) -> PathBuf {
        root.join(&self.0)
    }

    pub(super) fn prepare(&self, root: &FsPath) -> Result<()> {
        let output = self.output(root);
        let existing = Self::nearest(output.parent().unwrap_or(root))?;
        if !fs::canonicalize(existing)?.starts_with(root) {
            return Err(Error::InvalidSpec(
                "archive entry escapes through a symlink".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_link(&self, target: &FsPath) -> Result<()> {
        if target.is_absolute() {
            return Err(Error::InvalidSpec(
                "archive symlink target is absolute".into(),
            ));
        }
        let mut resolved = self.0.parent().unwrap_or(FsPath::new("")).to_owned();
        for component in target.components() {
            match component {
                Component::Normal(value) => resolved.push(value),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !resolved.pop() {
                        return Err(Error::InvalidSpec(
                            "archive symlink escapes extraction root".into(),
                        ));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(Error::InvalidSpec(
                        "archive symlink target is unsafe".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn replace(&self, root: &FsPath) -> Result<()> {
        let destination = self.output(root);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&destination)?,
            Ok(_) => fs::remove_file(&destination)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(super) fn ensure_dir(&self, root: &FsPath) -> Result<()> {
        let destination = self.output(root);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                fs::remove_file(&destination)?;
            }
            Ok(_) => fs::remove_dir_all(&destination)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::create_dir_all(destination)?;
        Ok(())
    }

    pub(super) fn nearest(mut path: &FsPath) -> Result<&FsPath> {
        while !path.exists() {
            path = path.parent().ok_or_else(|| {
                Error::InvalidSpec("container path has no existing ancestor".into())
            })?;
        }
        Ok(path)
    }
}
