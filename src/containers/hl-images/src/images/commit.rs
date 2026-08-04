use super::*;

impl Images {
    /// Commit a child image from a parent and an uncompressed filesystem diff.
    ///
    /// # Errors
    /// Returns an error for invalid metadata, missing parent content, or durable write failure.
    pub fn commit_child(
        &self,
        parent: &Image,
        mut diff: impl Read,
        name: &Reference,
        metadata: &Metadata,
    ) -> Result<Image> {
        metadata.runtime.validate()?;
        let manifest_descriptor = self.selected_manifest(&parent.target, &metadata.platform)?;
        let parent_manifest: ManifestDocument =
            serde_json::from_slice(&self.content.read_document(&manifest_descriptor)?)?;
        parent_manifest.validate()?;
        let parent_config_descriptor = parent_manifest.config.clone();
        let parent_config: ConfigDocument =
            serde_json::from_slice(&self.content.read_document(&parent_manifest.config)?)?;
        parent_config.require_platform(&metadata.platform)?;

        let mut plain = Vec::new();
        diff.read_to_end(&mut plain)?;
        let diff_id = Digest::sha256(&plain).to_string();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&plain)?;
        let compressed = encoder.finish()?;
        let layer = Blob::new(&compressed, MediaType::ImageLayerGzip).descriptor()?;

        let mut diff_ids = parent_config.rootfs.diff_ids;
        diff_ids.push(diff_id);
        let config_bytes = metadata.config_bytes(&diff_ids)?;
        let config = Blob::new(&config_bytes, MediaType::ImageConfig).descriptor()?;
        let mut layers = parent_manifest.layers;
        layers.push(layer.clone());
        let manifest_bytes = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": config,
            "layers": layers,
        }))?;
        let manifest = Blob::new(&manifest_bytes, MediaType::ImageManifest).descriptor()?;

        let lease = self.leases.create(BTreeMap::new())?;
        let result = (|| {
            for (descriptor, bytes) in [
                (&layer, compressed.as_slice()),
                (&config, config_bytes.as_slice()),
                (&manifest, manifest_bytes.as_slice()),
            ] {
                self.store_child_bytes(lease.id(), descriptor, bytes)?;
            }
            for descriptor in &layers[..layers.len() - 1] {
                self.leases
                    .add(lease.id(), format!("content:{}", descriptor.digest()))?;
            }
            for descriptor in [&manifest_descriptor, &parent_config_descriptor] {
                self.leases
                    .add(lease.id(), format!("content:{}", descriptor.digest()))?;
            }
            let image = Image {
                name: name.clone(),
                target: manifest,
            };
            self.metadata.put(image.clone())?;
            self.metadata
                .enrich(&image.target, metadata.created_at_ms(), metadata.labels.clone())?;
            Ok(image)
        })();
        let released = self.leases.delete(lease.id());
        match (result, released) {
            (Ok(image), Ok(_)) => Ok(image),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn selected_manifest(&self, target: &Descriptor, platform: &Platform) -> Result<Descriptor> {
        if target.is_index() {
            serde_json::from_slice::<IndexDocument>(&self.content.read_document(target)?)?.select_platform(platform)
        } else if target.is_manifest() {
            Ok(target.clone())
        } else {
            Err(Error::MalformedOci("parent target is not an image manifest".into()))
        }
    }

    fn store_child_bytes(&self, lease: &str, descriptor: &Descriptor, bytes: &[u8]) -> Result<()> {
        let mut ingest = self.content.ingest(format!("child-{}", descriptor.digest()))?;
        ingest.write(bytes)?;
        ingest.commit(descriptor)?;
        self.leases.add(lease, format!("content:{}", descriptor.digest()))
    }
    /// Import an uncompressed rootfs tar stream as a named single-layer image.
    ///
    /// The layer is spooled to a temporary file while hashing, so image size is
    /// bounded by available storage rather than process memory.
    ///
    /// # Errors
    /// Returns stream, archive, content, metadata, or reference failures.
    pub fn import(
        &self,
        layer: impl Read,
        runtime: &RuntimeConfig,
        platform: &Platform,
        name: &Reference,
    ) -> Result<Image> {
        let metadata = Metadata {
            platform: platform.clone(),
            created: None,
            author: None,
            labels: BTreeMap::new(),
            history: vec![History {
                created_by: Some("hl import".into()),
                ..History::default()
            }],
            runtime: runtime.clone(),
            onbuild: Vec::new(),
            exposed_ports: std::collections::BTreeSet::new(),
            volumes: std::collections::BTreeSet::new(),
            healthcheck: None,
            stop_signal: None,
        };
        self.ingest(layer, name, &metadata)
    }

    fn ingest(&self, mut layer: impl Read, name: &Reference, metadata: &Metadata) -> Result<Image> {
        let mut file = tempfile::tempfile()?;
        let mut digest = sha2::Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = layer.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            digest.update(&buffer[..read]);
            size = size
                .checked_add(read as u64)
                .ok_or_else(|| Error::MalformedOci("import layer size overflow".into()))?;
        }
        file.seek(SeekFrom::Start(0))?;
        let mut diff = String::from("sha256:");
        for byte in digest.finalize() {
            write!(diff, "{byte:02x}").expect("writing to a String cannot fail");
        }
        let environment = metadata
            .runtime
            .environment
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        let config = serde_json::to_vec(&serde_json::json!({
            "architecture": metadata.platform.architecture,
            "os": metadata.platform.os,
            "variant": metadata.platform.variant,
            "author": metadata.author,
            "config": {
                "Entrypoint": metadata.runtime.entrypoint,
                "Cmd": metadata.runtime.command,
                "Env": environment,
                "WorkingDir": metadata.runtime.working_directory,
                "User": metadata.runtime.user,
                "Labels": metadata.labels,
                "OnBuild": metadata.onbuild,
                "ExposedPorts": metadata.exposed_ports.iter().map(|port| (port, serde_json::Value::Object(serde_json::Map::new()))).collect::<BTreeMap<_, _>>(),
                "Volumes": metadata.volumes.iter().map(|path| (path, serde_json::Value::Object(serde_json::Map::new()))).collect::<BTreeMap<_, _>>(),
                "Healthcheck": metadata.healthcheck,
                "StopSignal": metadata.stop_signal,
            },
            "rootfs": {"type": "layers", "diff_ids": [diff]},
            "history": metadata.history,
        }))?;
        let manifest = serde_json::to_vec(&serde_json::json!([{
            "Config": "config.json",
            "RepoTags": [name.to_string()],
            "Layers": ["layer.tar"],
        }]))?;
        let mut outer = tempfile::tempfile()?;
        {
            let mut archive = tar::Builder::new(&mut outer);
            for (path, bytes) in [
                ("config.json", config.as_slice()),
                ("manifest.json", manifest.as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, path, bytes)?;
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(size);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, "layer.tar", &mut file)?;
            archive.finish()?;
        }
        outer.seek(SeekFrom::Start(0))?;
        let image = crate::format::docker::Archive::load(outer, self, crate::format::docker::Limits::default())?
            .into_iter()
            .next()
            .ok_or_else(|| Error::MalformedOci("import produced no image".into()))?;
        self.metadata
            .enrich(&image.target, metadata.created_at_ms(), metadata.labels.clone())?;
        Ok(image)
    }

    /// Create a named image from an uncompressed rootfs tar and runtime configuration.
    ///
    /// # Errors
    /// Returns archive, content, metadata, or reference validation failures.
    pub fn commit(
        &self,
        layer: &[u8],
        runtime: &RuntimeConfig,
        platform: &Platform,
        name: &Reference,
    ) -> Result<Image> {
        let metadata = Metadata {
            platform: platform.clone(),
            created: None,
            author: None,
            labels: BTreeMap::new(),
            history: vec![History {
                created_by: Some("hl commit".into()),
                ..History::default()
            }],
            runtime: runtime.clone(),
            onbuild: Vec::new(),
            exposed_ports: std::collections::BTreeSet::new(),
            volumes: std::collections::BTreeSet::new(),
            healthcheck: None,
            stop_signal: None,
        };
        self.ingest(Cursor::new(layer), name, &metadata)
    }

    /// Create a Dockerfile-built image with exact labels and instruction history.
    ///
    /// # Errors
    /// Returns archive, content, metadata, or reference validation failures.
    pub fn build(&self, layer: &[u8], name: &Reference, metadata: &Metadata) -> Result<Image> {
        self.ingest(Cursor::new(layer), name, metadata)
    }
}
