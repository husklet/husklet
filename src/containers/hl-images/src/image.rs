use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use crate::{storage, Descriptor, Error, Reference, Result};

const CATALOG_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub name: Reference,
    pub target: Descriptor,
}

/// Durable metadata for one immutable descriptor graph, independent of its current tags.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub target: Descriptor,
    pub names: BTreeSet<String>,
    pub created_at_ms: Option<u64>,
    pub labels: Option<BTreeMap<String, String>>,
    pub build_cache: bool,
    pub metadata_known: bool,
}

impl Graph {
    #[must_use]
    pub const fn filterable(&self) -> bool {
        self.metadata_known
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    generation: u64,
    images: BTreeMap<String, Image>,
    graphs: BTreeMap<String, Graph>,
    pending_prunes: BTreeMap<String, PendingPrune>,
}

impl Catalog {
    fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: CATALOG_VERSION,
                ..Self::default()
            });
        }
        let catalog: Catalog = serde_json::from_reader(File::open(path)?)?;
        if catalog.version != CATALOG_VERSION {
            return Err(Error::InvalidMetadata(format!(
                "unsupported image catalog version {}",
                catalog.version
            )));
        }
        Ok(catalog)
    }

    fn put(&mut self, image: Image) -> Option<Image> {
        let digest = image.target.digest().to_string();
        let name = image.name.to_string();
        let build_cache = image.name.repository().starts_with("hl-build-cache/");
        let target = image.target.clone();
        let previous = self.images.insert(name.clone(), image);
        self.detach_previous(previous.as_ref(), &digest, &name);
        self.graphs
            .entry(digest)
            .and_modify(|graph| {
                graph.names.insert(name.clone());
                graph.build_cache &= build_cache;
            })
            .or_insert_with(|| Graph {
                target,
                names: BTreeSet::from([name]),
                created_at_ms: None,
                labels: None,
                build_cache,
                metadata_known: false,
            });
        previous
    }

    fn detach_previous(&mut self, previous: Option<&Image>, digest: &str, name: &str) {
        let Some(previous) = previous else {
            return;
        };
        let previous_digest = previous.target.digest().to_string();
        if previous_digest == digest {
            return;
        }
        if let Some(graph) = self.graphs.get_mut(&previous_digest) {
            graph.names.remove(name);
        }
    }

    fn stage_prune(
        &mut self,
        generation: u64,
        digests: &BTreeSet<String>,
        content: BTreeMap<String, u64>,
    ) -> Result<Option<(String, PendingPrune)>> {
        if self.generation != generation {
            return Err(Error::InvalidMetadata(
                "image catalog changed while prune was being planned; retry".into(),
            ));
        }
        let removable = digests
            .iter()
            .filter(|digest| {
                self.graphs
                    .get(*digest)
                    .is_some_and(|graph| graph.names.is_empty() || graph.build_cache)
            })
            .cloned()
            .collect::<Vec<_>>();
        if removable.is_empty() {
            return Ok(None);
        }
        for digest in removable {
            self.remove_graph(&digest);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let prune = PendingPrune { content };
        self.pending_prunes.insert(id.clone(), prune.clone());
        Ok(Some((id, prune)))
    }

    fn remove_graph(&mut self, digest: &str) -> Option<Descriptor> {
        let graph = self.graphs.remove(digest)?;
        for name in graph.names {
            self.images.remove(&name);
        }
        Some(graph.target)
    }

    fn remove_graphs(&mut self, digests: &BTreeSet<String>) -> Vec<Descriptor> {
        let mut removed = Vec::new();
        for digest in digests {
            let removable = self
                .graphs
                .get(digest)
                .is_some_and(|graph| graph.names.is_empty() || graph.build_cache);
            if removable {
                removed.extend(self.remove_graph(digest));
            }
        }
        removed
    }

    fn remove(&mut self, name: &Reference) -> Option<Image> {
        let name = name.to_string();
        let removed = self.images.remove(&name)?;
        let digest = removed.target.digest().to_string();
        let Some(graph) = self.graphs.get_mut(&digest) else {
            return Some(removed);
        };
        graph.names.remove(&name);
        if !graph.names.is_empty() {
            graph.build_cache = graph.names.iter().all(|name| {
                name.parse::<Reference>()
                    .is_ok_and(|name| name.repository().starts_with("hl-build-cache/"))
            });
        }
        Some(removed)
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct PendingPrune {
    pub(crate) content: BTreeMap<String, u64>,
}

pub trait ImageStore: Clone + Send + Sync + 'static {
    /// # Errors
    /// Returns an error when metadata cannot be read.
    fn get(&self, name: &Reference) -> Result<Option<Image>>;
    /// # Errors
    /// Returns an error when the atomic metadata update fails.
    fn put(&self, image: Image) -> Result<Option<Image>>;
    /// # Errors
    /// Returns an error when the atomic metadata update fails.
    fn remove(&self, name: &Reference) -> Result<Option<Image>>;
    /// # Errors
    /// Returns an error when metadata cannot be read.
    fn list(&self) -> Result<Vec<Image>>;
}

#[derive(Clone)]
pub struct FsImageStore {
    path: PathBuf,
    state: Arc<RwLock<Catalog>>,
    writers: Arc<Mutex<()>>,
    persistence: Arc<dyn storage::Persistence>,
}

impl std::fmt::Debug for FsImageStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FsImageStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FsImageStore {
    /// # Errors
    /// Returns an error when metadata storage cannot be opened or decoded.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(root, Arc::new(storage::Native))
    }

    /// Open metadata using an explicit durable filesystem implementation.
    ///
    /// # Errors
    /// Returns an error when metadata storage cannot be opened or decoded.
    pub fn open_with(
        root: impl AsRef<Path>,
        persistence: Arc<dyn storage::Persistence>,
    ) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let path = root.as_ref().join("images.json");
        let state = Catalog::read(&path)?;
        let writers = storage::Writers::for_path(&path)?;
        Ok(Self {
            path,
            state: Arc::new(RwLock::new(state)),
            writers,
            persistence,
        })
    }

    fn update<T>(&self, operation: impl FnOnce(&mut Catalog) -> T) -> Result<T> {
        self.try_update(|state| Ok(operation(state)))
    }

    fn try_update<T>(&self, operation: impl FnOnce(&mut Catalog) -> Result<T>) -> Result<T> {
        let _writer = self
            .writers
            .lock()
            .map_err(|_| Error::InvalidMetadata("image writer lock poisoned".into()))?;
        let _process = storage::ExclusiveLock::acquire(&self.path.with_extension("lock"))?;
        let mut state = self
            .state
            .write()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?;
        // Another independently-opened store may have committed while this handle was idle.
        // Reload under the path-wide writer lock so concurrent tags cannot overwrite each other.
        let mut candidate = Catalog::read(&self.path)?;
        let result = operation(&mut candidate)?;
        candidate.generation = candidate.generation.wrapping_add(1);
        self.persistence
            .replace(&self.path, &serde_json::to_vec(&candidate)?)?;
        *state = candidate;
        Ok(result)
    }

    fn refresh(&self) -> Result<()> {
        let _writer = self
            .writers
            .lock()
            .map_err(|_| Error::InvalidMetadata("image writer lock poisoned".into()))?;
        let _process = storage::ExclusiveLock::acquire(&self.path.with_extension("lock"))?;
        *self
            .state
            .write()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))? =
            Catalog::read(&self.path)?;
        Ok(())
    }

    /// Atomically publish multiple image records in one metadata replacement.
    ///
    /// # Errors
    /// Returns an error when the metadata lock or durable atomic write fails.
    pub fn put_all(&self, images: impl IntoIterator<Item = Image>) -> Result<Vec<Option<Image>>> {
        let images: Vec<Image> = images.into_iter().collect();
        self.update(|state| images.into_iter().map(|image| state.put(image)).collect())
    }

    /// Snapshot the durable descriptor-graph catalog.
    ///
    /// # Errors
    /// Returns an error if the catalog lock is poisoned.
    pub fn graphs(&self) -> Result<Vec<Graph>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?
            .graphs
            .values()
            .cloned()
            .collect())
    }

    pub(crate) fn graph_snapshot(&self) -> Result<(u64, Vec<Graph>)> {
        self.refresh()?;
        let state = self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?;
        Ok((state.generation, state.graphs.values().cloned().collect()))
    }

    pub(crate) fn pending_prunes(&self) -> Result<Vec<(String, PendingPrune)>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?
            .pending_prunes
            .iter()
            .map(|(id, prune)| (id.clone(), prune.clone()))
            .collect())
    }

    pub(crate) fn stage_prune(
        &self,
        generation: u64,
        digests: &BTreeSet<String>,
        content: BTreeMap<String, u64>,
    ) -> Result<Option<(String, PendingPrune)>> {
        self.try_update(|state| state.stage_prune(generation, digests, content))
    }

    pub(crate) fn finish_prune(&self, id: &str) -> Result<()> {
        self.update(|state| {
            state.pending_prunes.remove(id);
        })
    }

    /// Attach filter metadata to a graph in the same transaction as the catalog.
    ///
    /// # Errors
    /// Returns an error if the catalog cannot be durably replaced.
    pub fn enrich(
        &self,
        target: &Descriptor,
        created_at_ms: Option<u64>,
        labels: BTreeMap<String, String>,
    ) -> Result<bool> {
        self.update(|state| {
            let Some(graph) = state.graphs.get_mut(&target.digest().to_string()) else {
                return false;
            };
            graph.created_at_ms = created_at_ms;
            graph.labels = Some(labels);
            graph.metadata_known = true;
            true
        })
    }

    /// Publish a name and its filterable graph metadata in one durable catalog replacement.
    ///
    /// # Errors
    /// Returns an error if the catalog cannot be durably replaced.
    pub fn publish(
        &self,
        image: Image,
        created_at_ms: Option<u64>,
        labels: BTreeMap<String, String>,
    ) -> Result<Option<Image>> {
        let digest = image.target.digest().to_string();
        self.try_update(|state| {
            let previous = state.put(image);
            let graph = state
                .graphs
                .get_mut(&digest)
                .ok_or_else(|| Error::InvalidMetadata("published image graph is missing".into()))?;
            graph.created_at_ms = created_at_ms;
            graph.labels = Some(labels);
            graph.metadata_known = true;
            Ok(previous)
        })
    }

    /// Forget selected untagged graphs atomically and return their descriptor roots.
    ///
    /// # Errors
    /// Returns an error if the catalog cannot be durably replaced.
    pub fn remove_graphs(&self, digests: &BTreeSet<String>) -> Result<Vec<Descriptor>> {
        self.update(|state| state.remove_graphs(digests))
    }
}

impl ImageStore for FsImageStore {
    fn get(&self, name: &Reference) -> Result<Option<Image>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?
            .images
            .get(&name.to_string())
            .cloned())
    }
    fn put(&self, image: Image) -> Result<Option<Image>> {
        self.update(|state| state.put(image))
    }
    fn remove(&self, name: &Reference) -> Result<Option<Image>> {
        self.update(|state| state.remove(name))
    }
    fn list(&self) -> Result<Vec<Image>> {
        self.refresh()?;
        Ok(self
            .state
            .read()
            .map_err(|_| Error::InvalidMetadata("image metadata lock poisoned".into()))?
            .images
            .values()
            .cloned()
            .collect())
    }
}
