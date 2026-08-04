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
    pub fn open_with(root: impl AsRef<Path>, persistence: Arc<dyn crate::storage::Persistence>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)?;
        Ok(Self {
            content: FsStore::open_with(root.join("content"), persistence.clone())?,
            metadata: FsImageStore::open_with(root.join("metadata"), persistence)?,
            leases: Leases::open(root.join("metadata"))?,
            snapshots: Snapshots::open(root.join("snapshots"))?,
            operation_lock: root.join("operations.lock"),
            fallback: None,
            pull_target: None,
        })
    }

    /// Creates a workspace-first catalog whose registry pulls are stored in the external catalog.
    #[must_use]
    pub fn workspace(mut workspace: Self, external: Self) -> Self {
        let external = Arc::new(external);
        workspace.fallback = Some(Arc::clone(&external));
        workspace.pull_target = Some(external);
        workspace
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
        match self.metadata.get(reference)? {
            Some(image) => Ok(Some(image)),
            None => self
                .fallback
                .as_deref()
                .map_or(Ok(None), |fallback| fallback.resolve(reference)),
        }
    }

    /// Lists workspace images first, followed by external images not shadowed by workspace names.
    ///
    /// # Errors
    /// Returns an error when either catalog cannot be read.
    pub fn list(&self) -> Result<Vec<Image>> {
        let mut images = self.metadata.list()?;
        let Some(fallback) = &self.fallback else {
            return Ok(images);
        };
        let local = images
            .iter()
            .map(|image| image.name.to_string())
            .collect::<HashSet<_>>();
        images.extend(
            fallback
                .list()?
                .into_iter()
                .filter(|image| !local.contains(&image.name.to_string())),
        );
        Ok(images)
    }

    /// Returns graph metadata across the workspace and external catalogs.
    ///
    /// # Errors
    /// Returns an error when either catalog cannot be read.
    pub fn graphs(&self) -> Result<Vec<Graph>> {
        let mut graphs = self.metadata.graphs()?;
        if let Some(fallback) = &self.fallback {
            let local = graphs
                .iter()
                .map(|graph| graph.target.digest().to_string())
                .collect::<HashSet<_>>();
            graphs.extend(
                fallback
                    .graphs()?
                    .into_iter()
                    .filter(|graph| !local.contains(&graph.target.digest().to_string())),
            );
        }
        Ok(graphs)
    }

    pub(super) fn mirror(&self, image: &Image) -> Result<()> {
        self.mirror_target(&image.target)
    }

    pub(super) fn mirror_target(&self, target: &Descriptor) -> Result<()> {
        use std::io::Read as _;

        if self.content.reader(target).is_ok() {
            return Ok(());
        }
        let source = self
            .fallback
            .as_deref()
            .ok_or_else(|| Error::ContentNotFound(target.digest().to_string()))?;
        let source = source.owner_target(target)?;
        for descriptor in DescriptorGraph::walk(target.clone(), &source.content)? {
            let digest: Digest = descriptor.digest().to_string().parse()?;
            if self.content.contains(&digest)? {
                continue;
            }
            let mut bytes = Vec::with_capacity(usize::try_from(descriptor.size()).unwrap_or(0));
            source.content.reader(&descriptor)?.read_to_end(&mut bytes)?;
            let mut draft = self.content.ingest(format!("workspace-{digest}"))?;
            draft.write(&bytes)?;
            draft.commit(&descriptor)?;
        }
        Ok(())
    }

    fn owner_target(&self, target: &Descriptor) -> Result<&Images> {
        if self.content.reader(target).is_ok() {
            return Ok(self);
        }
        self.fallback
            .as_deref()
            .ok_or_else(|| Error::ContentNotFound(target.digest().to_string()))?
            .owner_target(target)
    }

    /// Return the deduplicated compressed byte size of an image's complete descriptor graph.
    ///
    /// # Errors
    /// Returns an error when referenced content is missing, corrupt, or malformed.
    pub fn size(&self, image: &Image) -> Result<u64> {
        self.size_target(&image.target)
    }

    /// Return the deduplicated compressed size of a durable graph target.
    ///
    /// # Errors
    /// Returns an error when referenced content is missing, corrupt, or malformed.
    pub fn size_target(&self, target: &Descriptor) -> Result<u64> {
        self.mirror_target(target)?;
        crate::DescriptorGraph::walk(target.clone(), &self.content)?
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
        self.usage_targets(&images.iter().map(|image| image.target.clone()).collect::<Vec<_>>())
    }

    /// Account total and shared bytes for distinct durable graph targets.
    ///
    /// # Errors
    /// Returns an error when referenced content is missing, corrupt, or malformed.
    pub fn usage_targets(&self, targets: &[Descriptor]) -> Result<BTreeMap<String, ImageUsage>> {
        for target in targets {
            self.mirror_target(target)?;
        }
        let mut graphs = BTreeMap::new();
        for descriptor in targets {
            let target = descriptor.digest().to_string();
            if graphs.contains_key(&target) {
                continue;
            }
            let descriptors = crate::DescriptorGraph::walk(descriptor.clone(), &self.content)?
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
                        shared = shared
                            .checked_add(bytes)
                            .ok_or_else(|| Error::MalformedOci("shared image size overflow".into()))?;
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
        self.metadata.remove_target(&image.target.digest().to_string())
    }
}
