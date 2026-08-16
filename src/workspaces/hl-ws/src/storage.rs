//! Durable byte storage below one workspace-owned namespace.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static REPLACEMENT: AtomicU64 = AtomicU64::new(0);

/// A validated, relative, `/`-separated storage key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key(String);

impl Key {
    /// Parses a non-empty relative key without traversal components.
    ///
    /// # Errors
    /// Returns an error for absolute, empty, current-directory, parent-directory, or NUL components.
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.starts_with('/') {
            return Err(Error::InvalidKey(value));
        }
        if value.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || component.as_bytes().contains(&0)
        }) {
            return Err(Error::InvalidKey(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns a child key inside this key.
    ///
    /// # Errors
    /// Returns an error when `suffix` is not a valid key.
    pub fn join(&self, suffix: impl Into<String>) -> Result<Self, Error> {
        let suffix = Self::parse(suffix)?;
        Ok(Self(format!("{}/{}", self.0, suffix.0)))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Failure from workspace byte storage.
#[derive(Debug)]
pub enum Error {
    Deadline,
    InvalidKey(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => formatter.write_str("workspace storage deadline exceeded"),
            Self::InvalidKey(key) => write!(formatter, "invalid workspace storage key: {key:?}"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Deadline => None,
            Self::InvalidKey(_) => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Durable key/value bytes used by workspace-owned capabilities.
pub trait Storage: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn put(&self, key: &Key, bytes: &[u8]) -> Result<(), Self::Error>;
    fn get(&self, key: &Key) -> Result<Vec<u8>, Self::Error>;
    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Key>, Self::Error>;
    fn list_until(&self, prefix: Option<&Key>, deadline: Instant) -> Result<Vec<Key>, Self::Error>;
    fn remove(&self, key: &Key) -> Result<(), Self::Error>;
    fn remove_until(&self, key: &Key, deadline: Instant) -> Result<(), Self::Error>;
}

/// A storage view that transparently places every operation below one key.
#[derive(Clone, Debug)]
pub struct Namespace<S> {
    storage: S,
    prefix: Key,
}

impl<S> Namespace<S> {
    #[must_use]
    pub fn new(storage: S, prefix: Key) -> Self {
        Self { storage, prefix }
    }

    #[must_use]
    pub fn prefix(&self) -> &Key {
        &self.prefix
    }
}

impl<S: Storage> Storage for Namespace<S> {
    type Error = S::Error;

    fn put(&self, key: &Key, bytes: &[u8]) -> Result<(), Self::Error> {
        self.storage
            .put(&self.prefix.join(key.as_str()).expect("validated keys join"), bytes)
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>, Self::Error> {
        self.storage
            .get(&self.prefix.join(key.as_str()).expect("validated keys join"))
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Key>, Self::Error> {
        let absolute = match prefix {
            Some(prefix) => self.prefix.join(prefix.as_str()).expect("validated keys join"),
            None => self.prefix.clone(),
        };
        let base = format!("{}/", self.prefix.as_str());
        self.storage.list(Some(&absolute)).map(|keys| {
            keys.into_iter()
                .filter_map(|key| {
                    key.as_str()
                        .strip_prefix(&base)
                        .and_then(|relative| Key::parse(relative).ok())
                })
                .collect()
        })
    }

    fn list_until(&self, prefix: Option<&Key>, deadline: Instant) -> Result<Vec<Key>, Self::Error> {
        let absolute = match prefix {
            Some(prefix) => self.prefix.join(prefix.as_str()).expect("validated keys join"),
            None => self.prefix.clone(),
        };
        let base = format!("{}/", self.prefix.as_str());
        self.storage.list_until(Some(&absolute), deadline).map(|keys| {
            keys.into_iter()
                .filter_map(|key| {
                    key.as_str()
                        .strip_prefix(&base)
                        .and_then(|relative| Key::parse(relative).ok())
                })
                .collect()
        })
    }

    fn remove(&self, key: &Key) -> Result<(), Self::Error> {
        self.storage
            .remove(&self.prefix.join(key.as_str()).expect("validated keys join"))
    }

    fn remove_until(&self, key: &Key, deadline: Instant) -> Result<(), Self::Error> {
        self.storage
            .remove_until(&self.prefix.join(key.as_str()).expect("validated keys join"), deadline)
    }
}

/// Filesystem-backed workspace storage rooted at one directory.
#[derive(Clone, Debug)]
pub struct Directory {
    root: PathBuf,
}

impl Directory {
    /// Opens or creates a durable storage root.
    ///
    /// # Errors
    /// Returns an error when the root cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, key: &Key) -> Result<PathBuf, Error> {
        self.path_for_checked(key, &mut || Ok(()))
    }

    fn path_for_checked(&self, key: &Key, check: &mut impl FnMut() -> Result<(), Error>) -> Result<PathBuf, Error> {
        let mut path = self.root.clone();
        for component in key.as_str().split('/') {
            check()?;
            path.push(component);
            if path.exists() && std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::InvalidKey(key.to_string()));
            }
        }
        check()?;
        Ok(path)
    }

    fn collect_checked(
        &self,
        directory: &Path,
        keys: &mut Vec<Key>,
        check: &mut impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        check()?;
        if !directory.exists() {
            return Ok(());
        }
        check()?;
        for entry in std::fs::read_dir(directory)? {
            check()?;
            let entry = entry?;
            check()?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                self.collect_checked(&entry.path(), keys, check)?;
            } else if kind.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|_| Error::InvalidKey(entry.path().display().to_string()))?
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                keys.push(Key::parse(relative)?);
            }
        }
        check()?;
        Ok(())
    }

    fn collect(&self, directory: &Path, keys: &mut Vec<Key>) -> Result<(), Error> {
        self.collect_checked(directory, keys, &mut || Ok(()))
    }
}

impl Storage for Directory {
    type Error = Error;

    fn put(&self, key: &Key, bytes: &[u8]) -> Result<(), Self::Error> {
        let path = self.path_for(key)?;
        let parent = path.parent().ok_or_else(|| Error::InvalidKey(key.to_string()))?;
        std::fs::create_dir_all(parent)?;
        let replacement = path.with_extension(format!(
            "replace-{}-{}",
            std::process::id(),
            REPLACEMENT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = std::fs::write(&replacement, bytes).and_then(|()| std::fs::rename(&replacement, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(&replacement);
        }
        result.map_err(Error::from)
    }

    fn get(&self, key: &Key) -> Result<Vec<u8>, Self::Error> {
        std::fs::read(self.path_for(key)?).map_err(Error::from)
    }

    fn list(&self, prefix: Option<&Key>) -> Result<Vec<Key>, Self::Error> {
        let directory = match prefix {
            Some(prefix) => self.path_for(prefix)?,
            None => self.root.clone(),
        };
        let mut keys = Vec::new();
        self.collect(&directory, &mut keys)?;
        keys.sort();
        Ok(keys)
    }

    fn list_until(&self, prefix: Option<&Key>, deadline: Instant) -> Result<Vec<Key>, Self::Error> {
        let directory = match prefix {
            Some(prefix) => self.path_for(prefix)?,
            None => self.root.clone(),
        };
        let mut keys = Vec::new();
        self.collect_checked(&directory, &mut keys, &mut || {
            (Instant::now() < deadline).then_some(()).ok_or(Error::Deadline)
        })?;
        keys.sort();
        Ok(keys)
    }

    fn remove(&self, key: &Key) -> Result<(), Self::Error> {
        match std::fs::remove_file(self.path_for(key)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn remove_until(&self, key: &Key, deadline: Instant) -> Result<(), Self::Error> {
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        let path = self.path_for_checked(key, &mut || {
            (Instant::now() < deadline).then_some(()).ok_or(Error::Deadline)
        })?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "hl-ws-storage-{name}-{}-{}",
                std::process::id(),
                REPLACEMENT.fetch_add(1, Ordering::Relaxed)
            ));
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn namespace_confines_and_relativizes_keys() {
        let temporary = TestDirectory::new("namespace");
        let directory = Directory::open(&temporary.path).unwrap();
        let checkpoint = Namespace::new(directory.clone(), Key::parse("checkpoint/current").unwrap());
        checkpoint.put(&Key::parse("proc.7/pages").unwrap(), b"pages").unwrap();
        directory
            .put(&Key::parse("settings/workspace").unwrap(), b"settings")
            .unwrap();

        assert_eq!(checkpoint.get(&Key::parse("proc.7/pages").unwrap()).unwrap(), b"pages");
        assert_eq!(checkpoint.list(None).unwrap(), [Key::parse("proc.7/pages").unwrap()]);
        assert_eq!(
            std::fs::read(temporary.path.join("checkpoint/current/proc.7/pages")).unwrap(),
            b"pages"
        );
    }

    #[test]
    fn key_rejects_traversal_and_ambiguous_components() {
        for key in ["", "/absolute", "../outside", "a/../b", "a//b", "./a"] {
            assert!(Key::parse(key).is_err(), "{key}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_never_follows_symlinks() {
        let temporary = TestDirectory::new("symlink");
        let outside = TestDirectory::new("outside");
        let directory = Directory::open(&temporary.path).unwrap();
        std::os::unix::fs::symlink(&outside.path, temporary.path.join("escape")).unwrap();

        assert!(directory.put(&Key::parse("escape/value").unwrap(), b"bad").is_err());
        assert!(!outside.path.join("value").exists());
    }

    #[test]
    fn collection_checks_deadline_during_traversal() {
        let temporary = TestDirectory::new("collection-deadline");
        let directory = Directory::open(&temporary.path).unwrap();
        directory.put(&Key::parse("one/value").unwrap(), b"one").unwrap();
        directory.put(&Key::parse("two/value").unwrap(), b"two").unwrap();
        let mut checks = 0;
        let mut keys = Vec::new();

        let error = directory
            .collect_checked(&temporary.path, &mut keys, &mut || {
                checks += 1;
                (checks < 4).then_some(()).ok_or(Error::Deadline)
            })
            .unwrap_err();

        assert!(matches!(error, Error::Deadline));
        assert_eq!(checks, 4);
    }

    #[test]
    fn expired_remove_preserves_object() {
        let temporary = TestDirectory::new("remove-deadline");
        let directory = Directory::open(&temporary.path).unwrap();
        let key = Key::parse("checkpoint/object").unwrap();
        directory.put(&key, b"value").unwrap();

        assert!(matches!(
            directory.remove_until(&key, Instant::now()),
            Err(Error::Deadline)
        ));
        assert_eq!(directory.get(&key).unwrap(), b"value");
    }
}
