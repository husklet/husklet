use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Failure from durable checkpoint object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointError {
    message: String,
}

impl CheckpointError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CheckpointError {}

/// One complete, named process-tree checkpoint image.
pub trait CheckpointImage: Send + Sync {
    /// Stores one object in the unpublished checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage or object-name failure.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError>;

    /// Reads one object from the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage, object-name, or missing-object failure.
    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError>;

    /// Lists objects in the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn list(&self) -> Result<Vec<String>, CheckpointError>;

    /// Publishes a complete generation after its manifest is durable.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        self.put("MANIFEST", manifest)
    }
}

/// Opens checkpoint images by stable container generation namespace.
pub trait CheckpointImages: Send + Sync {
    /// Opens one isolated checkpoint generation stream.
    ///
    /// # Errors
    /// Returns an invalid-namespace or storage failure.
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError>;
}

pub(crate) struct DirectoryImages {
    root: PathBuf,
}

impl DirectoryImages {
    pub(crate) fn open(root: PathBuf) -> Result<Self, CheckpointError> {
        std::fs::create_dir_all(&root)
            .map_err(|error| CheckpointError::new(format!("create checkpoint root: {error}")))?;
        Ok(Self { root })
    }
}

impl CheckpointImages for DirectoryImages {
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError> {
        if namespace.is_empty()
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(CheckpointError::new("invalid checkpoint namespace"));
        }
        let root = self.root.join(namespace);
        std::fs::create_dir_all(&root)
            .map_err(|error| CheckpointError::new(format!("create checkpoint image: {error}")))?;
        Ok(Arc::new(DirectoryImage { root }))
    }
}

struct DirectoryImage {
    root: PathBuf,
}

impl DirectoryImage {
    fn path(&self, name: &str) -> Result<PathBuf, CheckpointError> {
        let path = Path::new(name);
        if path.is_absolute()
            || path.components().any(
                |component| !matches!(component, Component::Normal(value) if !value.is_empty()),
            )
        {
            return Err(CheckpointError::new(format!(
                "invalid checkpoint object name: {name:?}"
            )));
        }
        Ok(self.root.join(path))
    }

    fn collect(&self, directory: &Path, objects: &mut Vec<String>) -> Result<(), CheckpointError> {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| CheckpointError::new(format!("list checkpoint objects: {error}")))?
        {
            let entry = entry.map_err(|error| {
                CheckpointError::new(format!("read checkpoint object: {error}"))
            })?;
            if entry
                .file_type()
                .map_err(|error| CheckpointError::new(error.to_string()))?
                .is_dir()
            {
                self.collect(&entry.path(), objects)?;
            } else {
                let relative = entry
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|error| CheckpointError::new(error.to_string()))?
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                objects.push(relative);
            }
        }
        Ok(())
    }
}

impl CheckpointImage for DirectoryImage {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError> {
        let path = self.path(name)?;
        let parent = path
            .parent()
            .ok_or_else(|| CheckpointError::new("checkpoint object has no parent"))?;
        std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(path, bytes))
            .map_err(|error| CheckpointError::new(error.to_string()))
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError> {
        std::fs::read(self.path(name)?)
            .map_err(|error| CheckpointError::new(format!("read checkpoint object: {error}")))
    }

    fn list(&self) -> Result<Vec<String>, CheckpointError> {
        let mut objects = Vec::new();
        self.collect(&self.root, &mut objects)?;
        objects.sort();
        Ok(objects)
    }
}

pub(crate) struct EngineImage(Arc<dyn CheckpointImage>);

impl EngineImage {
    pub(crate) fn new(image: Arc<dyn CheckpointImage>) -> Self {
        Self(image)
    }
}

impl hl_engine::CheckpointStore for EngineImage {
    fn put(&self, name: &str, data: &[u8]) -> Result<(), hl_engine::StoreError> {
        self.0
            .put(name, data)
            .map_err(|error| hl_engine::StoreError::new(error.to_string()))
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, hl_engine::StoreError> {
        self.0
            .get(name)
            .map_err(|error| hl_engine::StoreError::new(error.to_string()))
    }

    fn list(&self) -> Result<Vec<String>, hl_engine::StoreError> {
        self.0
            .list()
            .map_err(|error| hl_engine::StoreError::new(error.to_string()))
    }

    fn commit(&self, manifest: &[u8]) -> Result<(), hl_engine::StoreError> {
        self.0
            .commit(manifest)
            .map_err(|error| hl_engine::StoreError::new(error.to_string()))
    }
}
