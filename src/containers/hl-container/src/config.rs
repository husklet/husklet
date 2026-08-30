use crate::Result;
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum TranslationCacheError {
    #[error("translation cache path must be absolute: {}", .0.display())]
    Relative(PathBuf),
    #[error("translation cache path must not be a symlink: {}", .0.display())]
    Symlink(PathBuf),
    #[error("translation cache path must be a directory: {}", .0.display())]
    NotDirectory(PathBuf),
    #[error("could not create translation cache directory {}: {source}", path.display())]
    Create { path: PathBuf, source: std::io::Error },
    #[error("could not inspect translation cache path {}: {source}", path.display())]
    Inspect { path: PathBuf, source: std::io::Error },
    #[error("could not verify translation cache directory {}: {source}", path.display())]
    Verify { path: PathBuf, source: std::io::Error },
    #[error("could not securely open translation cache directory {}: {source}", path.display())]
    Open { path: PathBuf, source: nix::Error },
    #[error("could not make translation cache directory private {}: {source}", path.display())]
    Permissions { path: PathBuf, source: std::io::Error },
}

#[derive(Clone, Debug)]
pub(crate) struct TranslationCache(PathBuf);

impl TranslationCache {
    fn new(directory: PathBuf) -> Self {
        Self(directory)
    }

    pub(crate) fn prepare(self) -> Result<PathBuf> {
        if !self.0.is_absolute() {
            return Err(TranslationCacheError::Relative(self.0).into());
        }

        match fs::symlink_metadata(&self.0) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(TranslationCacheError::Symlink(self.0).into());
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(TranslationCacheError::NotDirectory(self.0).into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder
                    .create(&self.0)
                    .map_err(|source| TranslationCacheError::Create {
                        path: self.0.clone(),
                        source,
                    })?;
            }
            Err(source) => {
                return Err(TranslationCacheError::Inspect { path: self.0, source }.into());
            }
        }

        let metadata = fs::symlink_metadata(&self.0).map_err(|source| TranslationCacheError::Verify {
            path: self.0.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(TranslationCacheError::Symlink(self.0).into());
        }
        if !metadata.is_dir() {
            return Err(TranslationCacheError::NotDirectory(self.0).into());
        }
        let directory = open(
            &self.0,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|source| TranslationCacheError::Open {
            path: self.0.clone(),
            source,
        })?;
        directory
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|source| TranslationCacheError::Permissions {
                path: self.0.clone(),
                source,
            })?;

        Ok(self.0)
    }
}

/// Persistence selected for container metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Persistence {
    /// Durable, versioned records below the configured state directory.
    File,
    /// Ephemeral state useful for embedded and test processes.
    Memory,
}

/// Configuration for a headless container service.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) root: PathBuf,
    pub(crate) persistence: Persistence,
    pub(crate) translation_cache: Option<TranslationCache>,
    pub(crate) translation_cache_observability: bool,
}

impl Config {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            persistence: Persistence::File,
            translation_cache: None,
            translation_cache_observability: false,
        }
    }

    #[must_use]
    pub fn persistence(mut self, value: Persistence) -> Self {
        self.persistence = value;
        self
    }

    /// Selects durable storage for translated engine code.
    #[must_use]
    pub fn translation_cache(mut self, directory: impl Into<PathBuf>) -> Self {
        self.translation_cache = Some(TranslationCache::new(directory.into()));
        self
    }

    /// Emits structured translated-cache diagnostics for explicit profiling runs.
    #[must_use]
    pub fn translation_cache_observability(mut self, enabled: bool) -> Self {
        self.translation_cache_observability = enabled;
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn translation_cache_requires_an_absolute_path() {
        let error = TranslationCache::new(PathBuf::from("relative/cache"))
            .prepare()
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid container configuration: translation cache path must be absolute: relative/cache"
        );
    }

    #[tokio::test]
    async fn service_construction_rejects_a_relative_translation_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let result = crate::Containers::builder(Config::new(temporary.path()).translation_cache("relative/cache"))
            .build()
            .await;
        let Err(error) = result else {
            panic!("relative translation cache unexpectedly accepted");
        };

        assert_eq!(
            error.to_string(),
            "invalid container configuration: translation cache path must be absolute: relative/cache"
        );
    }

    #[test]
    fn translation_cache_is_created_private() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("nested/cache");

        assert_eq!(TranslationCache::new(directory.clone()).prepare().unwrap(), directory);
        let mode = fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn translation_cache_makes_an_existing_directory_private() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("cache");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

        TranslationCache::new(directory.clone()).prepare().unwrap();

        let mode = fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn translation_cache_rejects_a_file() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("cache");
        fs::write(&path, b"not a directory").unwrap();

        let error = TranslationCache::new(path.clone()).prepare().unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "invalid container configuration: translation cache path must be a directory: {}",
                path.display()
            )
        );
    }

    #[test]
    fn translation_cache_rejects_a_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let path = temporary.path().join("cache");
        fs::create_dir(&target).unwrap();
        symlink(&target, &path).unwrap();

        let error = TranslationCache::new(path.clone()).prepare().unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "invalid container configuration: translation cache path must not be a symlink: {}",
                path.display()
            )
        );
    }
}
