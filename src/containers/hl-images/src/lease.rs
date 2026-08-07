use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use crate::{Error, Result, error::At as _, storage};
use storage::Persistence as _;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Lease {
    id: String,
    labels: BTreeMap<String, String>,
    resources: BTreeSet<String>,
}
impl Lease {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }
    #[must_use]
    pub fn owns(&self, resource: &str) -> bool {
        self.resources.contains(resource)
    }
    pub fn resources(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(String::as_str)
    }
}

pub trait LeaseStore: Clone + Send + Sync + 'static {
    /// # Errors
    /// Returns an error when durable lease metadata cannot be written.
    fn create(&self, labels: BTreeMap<String, String>) -> Result<Lease>;
    /// # Errors
    /// Returns an error when lease metadata cannot be read.
    fn get(&self, id: &str) -> Result<Option<Lease>>;
    /// # Errors
    /// Returns an error for an unknown lease or failed durable update.
    fn add(&self, id: &str, resource: impl Into<String>) -> Result<()>;
    /// # Errors
    /// Returns an error when the lease does not own the resource or persistence fails.
    fn remove(&self, id: &str, resource: &str) -> Result<()>;
    /// # Errors
    /// Returns an error when the durable update fails.
    fn delete(&self, id: &str) -> Result<Option<Lease>>;
    /// # Errors
    /// Returns an error when lease metadata cannot be read.
    fn list(&self) -> Result<Vec<Lease>>;
}

#[derive(Clone, Debug)]
pub struct Leases {
    path: PathBuf,
    state: Arc<RwLock<BTreeMap<String, Lease>>>,
    writers: Arc<Mutex<()>>,
}
impl Leases {
    /// # Errors
    /// Returns an error when lease storage cannot be opened or decoded.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(root.as_ref()).at(root.as_ref())?;
        let path = root.as_ref().join("leases.json");
        let writers = storage::Writers::for_path(&path)?;
        // Opening reads the same file commits replace, so it takes the same locks a commit does.
        let state = {
            let _writer = writers
                .lock()
                .map_err(|_| Error::InvalidMetadata("lease writer lock poisoned".into()))?;
            let _process = storage::ExclusiveLock::acquire(&path.with_extension("lock"))?;
            if path.exists() {
                serde_json::from_reader(File::open(&path).at(&path)?)?
            } else {
                BTreeMap::new()
            }
        };
        Ok(Self {
            writers,
            path,
            state: Arc::new(RwLock::new(state)),
        })
    }

    /// Create a lease with its initial resources in one durable transaction.
    ///
    /// # Errors
    /// Returns an error when lease metadata cannot be persisted.
    pub fn create_with(
        &self,
        labels: BTreeMap<String, String>,
        resources: impl IntoIterator<Item = String>,
    ) -> Result<Lease> {
        let lease = Lease {
            id: uuid::Uuid::new_v4().to_string(),
            labels,
            resources: resources.into_iter().collect(),
        };
        self.update(|state| {
            state.insert(lease.id.clone(), lease.clone());
            Ok(lease)
        })
    }

    fn update<T>(&self, operation: impl FnOnce(&mut BTreeMap<String, Lease>) -> Result<T>) -> Result<T> {
        let _writer = self
            .writers
            .lock()
            .map_err(|_| Error::InvalidMetadata("lease writer lock poisoned".into()))?;
        let _process = storage::ExclusiveLock::acquire(&self.path.with_extension("lock"))?;
        let mut state = self
            .state
            .write()
            .map_err(|_| Error::InvalidMetadata("lease metadata lock poisoned".into()))?;
        // Handles in other processes may have committed since this store opened.
        // Reload while holding the path-wide lock before applying the mutation.
        *state = if self.path.exists() {
            serde_json::from_reader(File::open(&self.path).at(&self.path)?)?
        } else {
            BTreeMap::new()
        };
        let result = operation(&mut state)?;
        storage::Native.replace(&self.path, &serde_json::to_vec(&*state)?)?;
        Ok(result)
    }

    fn refresh(&self) -> Result<()> {
        let _writer = self
            .writers
            .lock()
            .map_err(|_| Error::InvalidMetadata("lease writer lock poisoned".into()))?;
        let _process = storage::ExclusiveLock::acquire(&self.path.with_extension("lock"))?;
        let current = if self.path.exists() {
            serde_json::from_reader(File::open(&self.path).at(&self.path)?)?
        } else {
            BTreeMap::new()
        };
        *self
            .state
            .write()
            .map_err(|_| Error::InvalidMetadata("lease metadata lock poisoned".into()))? = current;
        Ok(())
    }
}
impl LeaseStore for Leases {
    fn create(&self, labels: BTreeMap<String, String>) -> Result<Lease> {
        self.create_with(labels, [])
    }
    fn get(&self, id: &str) -> Result<Option<Lease>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("lease metadata lock poisoned".into()))?
            .get(id)
            .cloned())
    }
    fn add(&self, id: &str, resource: impl Into<String>) -> Result<()> {
        let resource = resource.into();
        self.update(|state| {
            state
                .get_mut(id)
                .ok_or_else(|| Error::InvalidMetadata(format!("unknown lease {id}")))?
                .resources
                .insert(resource);
            Ok(())
        })
    }
    fn remove(&self, id: &str, resource: &str) -> Result<()> {
        self.update(|state| {
            let lease = state
                .get_mut(id)
                .ok_or_else(|| Error::InvalidMetadata(format!("unknown lease {id}")))?;
            if !lease.resources.remove(resource) {
                return Err(Error::NotOwned {
                    lease: id.into(),
                    resource: resource.into(),
                });
            }
            Ok(())
        })
    }
    fn delete(&self, id: &str) -> Result<Option<Lease>> {
        self.update(|state| Ok(state.remove(id)))
    }
    fn list(&self) -> Result<Vec<Lease>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("lease metadata lock poisoned".into()))?
            .values()
            .cloned()
            .collect())
    }
}
