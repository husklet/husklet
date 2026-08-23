use std::{
    fs,
    path::{Component, Path as FsPath, PathBuf},
};

use crate::{Error, Result};

#[cfg(test)]
thread_local! {
    static PREPARED_COMPONENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_prepared_components() -> usize {
    PREPARED_COMPONENTS.with(std::cell::Cell::take)
}

/// Validated relative path carried by an OCI layer entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path(PathBuf);

pub struct Parents(Vec<(PathBuf, fs::Permissions)>);

impl Drop for Parents {
    fn drop(&mut self) {
        for (path, permissions) in self.0.drain(..).rev() {
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

impl Path {
    pub(crate) fn normalize(path: &FsPath) -> Result<Self> {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::ParentDir => {
                    normalized.pop();
                }
                _ => {}
            }
        }
        Self::new(&normalized)
    }

    /// Validate and own a normalized relative archive path.
    ///
    /// # Errors
    /// Returns an unsafe-archive error for empty, absolute, or non-normalized paths.
    pub fn new(path: &FsPath) -> Result<Self> {
        let mut normalized = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(part) => normalized.push(part),
                _ => {
                    return Err(Self::error_at(path, "path is not a normalized relative path"));
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(Self::error_at(path, "path is not a normalized relative path"));
        }
        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_path(&self) -> &FsPath {
        &self.0
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.0.file_name().and_then(|value| value.to_str()).unwrap_or_default()
    }

    #[must_use]
    pub fn is_device(&self) -> bool {
        self.0.components().next().is_some_and(|part| part.as_os_str() == "dev")
    }

    /// Validate that a symlink target remains syntactically within the rootfs.
    ///
    /// # Errors
    /// Returns an unsafe-archive error when the target is malformed or escapes.
    pub fn validate_link(&self, target: &FsPath) -> Result<()> {
        if target.is_absolute() {
            if target
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            {
                return Err(self.error("invalid absolute symlink target"));
            }
            return Ok(());
        }
        let mut depth = self.0.parent().map_or(0, |parent| parent.components().count());
        for component in target.components() {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir if depth > 0 => depth -= 1,
                Component::ParentDir => return Err(self.error("symlink escapes rootfs")),
                _ => return Err(self.error("invalid symlink target")),
            }
        }
        Ok(())
    }

    /// Prepare every parent directory without traversing a symlink.
    ///
    /// # Errors
    /// Returns an error when an ancestor is a symlink/non-directory or cannot be created.
    pub fn prepare(&self, root: &FsPath) -> Result<Parents> {
        let mut current = root.to_owned();
        let mut changed = Vec::new();
        for component in self.0.parent().unwrap_or(FsPath::new("")).components() {
            #[cfg(test)]
            PREPARED_COMPONENTS.with(|counter| counter.set(counter.get() + 1));
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(self.error("entry traverses a symlink"));
                }
                Ok(meta) if !meta.is_dir() => {
                    return Err(self.error("entry parent is not a directory"));
                }
                Ok(metadata) => changed.extend(self.make_writable(&current, &metadata.permissions())?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current).map_err(|source| self.io("create parent directory", source))?;
                }
                Err(source) => return Err(self.io("inspect parent directory", source)),
            }
        }
        Ok(Parents(changed))
    }

    /// Grants owner write on a read-only parent, reporting the permissions to restore afterwards.
    fn make_writable(
        &self,
        current: &FsPath,
        original: &fs::Permissions,
    ) -> Result<Option<(PathBuf, fs::Permissions)>> {
        if !original.readonly() {
            return Ok(None);
        }
        let mut writable = original.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            writable.set_mode(original.mode() | 0o200);
        }
        #[cfg(not(unix))]
        writable.set_readonly(false);
        fs::set_permissions(current, writable).map_err(|source| self.io("make parent directory writable", source))?;
        Ok(Some((current.to_owned(), original.clone())))
    }

    /// Resolve and prepare the entry's parent below `root`.
    ///
    /// # Errors
    /// Returns an error when parent preparation fails.
    pub fn parent(&self, root: &FsPath) -> Result<PathBuf> {
        let parent = Self(self.0.parent().unwrap_or(FsPath::new("")).to_owned());
        let _parents = Self(parent.0.join("entry")).prepare(root)?;
        Ok(root.join(parent.0))
    }

    #[must_use]
    pub fn destination(&self, root: &FsPath) -> PathBuf {
        root.join(&self.0)
    }

    #[must_use]
    pub(crate) fn relative_to(&self, from: &FsPath) -> PathBuf {
        let from = from.components().collect::<Vec<_>>();
        let to = self.0.components().collect::<Vec<_>>();
        let shared = from.iter().zip(&to).take_while(|(left, right)| left == right).count();
        let mut path = PathBuf::new();
        for _ in shared..from.len() {
            path.push("..");
        }
        for component in &to[shared..] {
            path.push(component.as_os_str());
        }
        if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path
        }
    }

    /// Remove any existing filesystem node at `destination`.
    ///
    /// # Errors
    /// Returns filesystem failures.
    pub fn remove(&self, destination: &FsPath) -> Result<()> {
        if destination.symlink_metadata()?.is_dir() {
            fs::remove_dir_all(destination).map_err(|source| self.io("remove directory", source))?;
        } else {
            fs::remove_file(destination).map_err(|source| self.io("remove file", source))?;
        }
        Ok(())
    }

    /// Apply archive permission bits to `destination`.
    ///
    /// # Errors
    /// Returns filesystem failures.
    pub fn set_mode(&self, destination: &FsPath, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(destination, fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|source| self.io("set permissions", source))?;
        }
        #[cfg(not(unix))]
        let _ = (destination, mode);
        Ok(())
    }

    #[must_use]
    pub fn error(&self, reason: &'static str) -> Error {
        Self::error_at(&self.0, reason)
    }

    #[must_use]
    pub fn io(&self, operation: &'static str, source: std::io::Error) -> Error {
        Error::LayerFilesystem {
            operation,
            path: self.0.clone(),
            source,
        }
    }

    fn error_at(path: &FsPath, reason: &'static str) -> Error {
        Error::UnsafeArchive {
            path: path.to_owned(),
            reason,
        }
    }
}
