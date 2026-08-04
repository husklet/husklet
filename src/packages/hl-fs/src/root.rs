use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{FsError, Result};

/// Canonical host root for resolving untrusted relative paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// Opens and canonicalizes a directory root.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let requested = path.as_ref();
        let path = fs::canonicalize(requested).map_err(|error| FsError::io("canonicalize root", requested, error))?;
        let metadata = fs::metadata(&path).map_err(|error| FsError::io("read root metadata", &path, error))?;
        if !metadata.is_dir() {
            return Err(FsError::NotDirectory(path));
        }
        Ok(Self { path })
    }

    /// Canonical root path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolves a rooted relative path while checking every existing symlink ancestor.
    ///
    /// Missing trailing components are allowed. Safe `std` APIs cannot make a
    /// later use race-free; callers requiring authority-grade traversal need a
    /// platform handle adapter rather than a path.
    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        let components = Self::relative_components(relative)?;
        let mut candidate = self.path.clone();
        candidate.extend(&components);
        let mut existing = candidate.clone();
        let mut missing = Vec::<OsString>::new();
        loop {
            match fs::canonicalize(&existing) {
                Ok(canonical) => return self.finish_resolution(relative, canonical, &missing),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Self::ascend(&mut existing, &mut missing, &candidate, error)?
                }
                Err(error) => {
                    return Err(FsError::io("canonicalize rooted path", &existing, error));
                }
            }
        }
    }

    fn relative_components(relative: &Path) -> Result<Vec<OsString>> {
        relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(Ok(value.to_owned())),
                Component::CurDir => None,
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    Some(Err(FsError::PathEscape(relative.to_owned())))
                }
            })
            .collect()
    }

    fn ascend(
        existing: &mut PathBuf,
        missing: &mut Vec<OsString>,
        candidate: &Path,
        error: std::io::Error,
    ) -> Result<()> {
        let name = existing
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| FsError::io("canonicalize rooted path", existing, error))?;
        missing.push(name);
        if existing.pop() {
            Ok(())
        } else {
            Err(FsError::io(
                "canonicalize rooted path",
                candidate,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ))
        }
    }

    fn finish_resolution(&self, relative: &Path, canonical: PathBuf, missing: &[OsString]) -> Result<PathBuf> {
        if !canonical.starts_with(&self.path) {
            return Err(FsError::SymlinkEscape(relative.to_owned()));
        }
        let mut resolved = canonical;
        resolved.extend(missing.iter().rev());
        Ok(resolved)
    }

    /// Resolves a path and requires the final object to exist.
    pub fn resolve_existing(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        let resolved = self.resolve(relative)?;
        if !resolved.exists() {
            return Err(FsError::io(
                "resolve existing rooted path",
                &resolved,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        Ok(resolved)
    }
}
