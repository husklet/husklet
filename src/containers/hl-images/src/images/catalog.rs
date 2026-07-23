use super::*;

impl Images {
    /// Open the image stores rooted at `root`.
    ///
    /// # Errors
    /// Returns an error when a store cannot be created or decoded.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(root, Arc::new(crate::storage::Native))
    }

    /// Open all image stores over one explicit durable filesystem implementation.
    ///
    /// # Errors
    /// Returns an error when any durable store cannot be created or decoded.
    pub fn open_with(
        root: impl AsRef<Path>,
        persistence: Arc<dyn crate::storage::Persistence>,
    ) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        Ok(Self {
            content: FsStore::open_with(root.join("content"), persistence.clone())?,
            metadata: FsImageStore::open_with(root.join("metadata"), persistence)?,
            leases: Leases::open(root.join("metadata"))?,
            snapshots: Snapshots::open(root.join("snapshots"))?,
            operation_lock: root.join("operations.lock"),
        })
    }

    #[must_use]
    pub fn content(&self) -> &FsStore {
        &self.content
    }
    #[must_use]
    pub fn metadata(&self) -> &FsImageStore {
        &self.metadata
    }
    #[must_use]
    pub fn leases(&self) -> &Leases {
        &self.leases
    }

    /// Resolve a local image name without contacting a registry.
    ///
    /// # Errors
    /// Returns an error when image metadata cannot be read.
    pub fn resolve(&self, reference: &Reference) -> Result<Option<Image>> {
        self.metadata.get(reference)
    }

    /// Return the deduplicated compressed byte size of an image's complete descriptor graph.
    ///
    /// # Errors
    /// Returns an error when referenced content is missing, corrupt, or malformed.
    pub fn size(&self, image: &Image) -> Result<u64> {
        crate::DescriptorGraph::walk(image.target.clone(), &self.content)?
            .into_iter()
            .try_fold(0_u64, |total, descriptor| {
                total
                    .checked_add(descriptor.size())
                    .ok_or_else(|| Error::MalformedOci("image size overflow".into()))
            })
    }

    /// Account total and cross-image shared bytes for distinct immutable image graphs.
    ///
    /// Multiple tags of the same target remain one image and do not inflate sharing.
    ///
    /// # Errors
    /// Returns an error when referenced content is missing, corrupt, or malformed.
    pub fn usage(&self, images: &[Image]) -> Result<BTreeMap<String, ImageUsage>> {
        let mut graphs = BTreeMap::new();
        for image in images {
            let target = image.target.digest().to_string();
            if graphs.contains_key(&target) {
                continue;
            }
            let descriptors = crate::DescriptorGraph::walk(image.target.clone(), &self.content)?
                .into_iter()
                .map(|descriptor| (descriptor.digest().to_string(), descriptor.size()))
                .collect::<BTreeMap<_, _>>();
            graphs.insert(target, descriptors);
        }
        let mut references = BTreeMap::<String, usize>::new();
        for descriptors in graphs.values() {
            for digest in descriptors.keys() {
                *references.entry(digest.clone()).or_default() += 1;
            }
        }
        graphs
            .into_iter()
            .map(|(target, descriptors)| {
                let mut size = 0_u64;
                let mut shared = 0_u64;
                for (digest, bytes) in descriptors {
                    size = size
                        .checked_add(bytes)
                        .ok_or_else(|| Error::MalformedOci("image size overflow".into()))?;
                    if references.get(&digest).is_some_and(|count| *count > 1) {
                        shared = shared.checked_add(bytes).ok_or_else(|| {
                            Error::MalformedOci("shared image size overflow".into())
                        })?;
                    }
                }
                Ok((target, ImageUsage { size, shared }))
            })
            .collect()
    }

    /// Add or replace a name pointing to the same immutable target as `image`.
    ///
    /// # Errors
    /// Returns an error when the metadata update cannot be committed atomically.
    pub fn tag(&self, image: &Image, name: Reference) -> Result<Image> {
        let tagged = Image {
            name,
            target: image.target.clone(),
        };
        self.metadata.put(tagged.clone())?;
        Ok(tagged)
    }

    /// Remove one image name without deleting shared content.
    ///
    /// # Errors
    /// Returns an error when the metadata update cannot be committed atomically.
    pub fn remove(&self, reference: &Reference) -> Result<Option<Image>> {
        self.metadata.remove(reference)
    }

    /// Remove every local name for an image target without deleting shared content.
    ///
    /// This models Docker's forced image removal: aliases of the selected immutable
    /// target are untagged, while descriptor and layer reclamation remains the
    /// responsibility of the explicit garbage collector.
    ///
    /// # Errors
    /// Returns an error when a catalog update cannot be committed.
    pub fn force_remove(&self, image: &Image) -> Result<Vec<Image>> {
        let digest = image.target.digest();
        let aliases = self
            .metadata
            .list()?
            .into_iter()
            .filter(|candidate| candidate.target.digest() == digest)
            .map(|candidate| candidate.name)
            .collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(aliases.len());
        for alias in aliases {
            if let Some(image) = self.metadata.remove(&alias)? {
                removed.push(image);
            }
        }
        Ok(removed)
    }
}
