use super::*;

impl Images {
    /// Read the selected platform's runtime metadata for an image.
    ///
    /// # Errors
    /// Returns an error for missing, malformed, or platform-incompatible image content.
    pub fn details(&self, image: &Image, platform: &Platform) -> Result<Metadata> {
        let root = self.content.read_document(&image.target)?;
        let manifest = if image.target.is_index() {
            serde_json::from_slice::<IndexDocument>(&root)?.select_platform(platform)?
        } else {
            image.target.clone()
        };
        let document: ManifestDocument =
            serde_json::from_slice(&self.content.read_document(&manifest)?)
                .map_err(|error| Error::MalformedOci(error.to_string()))?;
        document.validate()?;
        let config: ConfigDocument =
            serde_json::from_slice(&self.content.read_document(&document.config)?)
                .map_err(|error| Error::MalformedOci(error.to_string()))?;
        config.require_platform(platform)?;
        let labels = config.config.labels.clone().unwrap_or_default();
        let onbuild = config.config.on_build.clone();
        let exposed_ports = config.config.exposed_ports.keys().cloned().collect();
        let volumes = config.config.volumes.keys().cloned().collect();
        let healthcheck = config.config.healthcheck.clone();
        let stop_signal = config.config.stop_signal.clone();
        let runtime = RuntimeConfig::try_from(config.config)?;
        Ok(Metadata {
            platform: Platform::new(config.os, config.architecture, platform.variant.clone()),
            created: config.created,
            author: config.author,
            labels,
            history: config.history,
            runtime,
            onbuild,
            exposed_ports,
            volumes,
            healthcheck,
            stop_signal,
        })
    }

    /// Create and pin a private writable rootfs derived from an unpacked image.
    ///
    /// # Errors
    /// Returns an error when snapshot validation or durable lease creation fails.
    pub fn rootfs(&self, image: &UnpackedImage) -> Result<RootReference> {
        let _operation = crate::storage::ExclusiveLock::acquire(&self.operation_lock)?;
        Roots::new(self.snapshots.clone(), self.leases.clone()).fork(image.snapshot())
    }

    #[must_use]
    pub fn roots(&self) -> Roots {
        Roots::new(self.snapshots.clone(), self.leases.clone())
    }
}
