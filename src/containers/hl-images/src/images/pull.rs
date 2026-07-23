use super::*;

impl Images {
    /// Pull and persist an image for the requested platform.
    ///
    /// # Errors
    /// Returns an error for registry, validation, lease, or durable storage failure.
    pub async fn pull(
        &self,
        source: &(impl Source + ?Sized),
        reference: Reference,
        platform: &Platform,
    ) -> Result<Image> {
        let _span = hl_log::hl_span!(hl_log::tag::IMAGE, "pull");
        hl_log::hl_info!(hl_log::tag::IMAGE, "pull begin reference={reference}");
        let lease = self
            .leases
            .create(BTreeMap::from([("kind".into(), "pull".into())]))?;
        let result = match self
            .pull_under_lease(source, &reference, platform, lease.id())
            .await
        {
            Ok(target) => (|| {
                let image = Image {
                    name: reference,
                    target,
                };
                let details = self.details(&image, platform)?;
                self.metadata
                    .publish(image.clone(), details.created_at_ms(), details.labels)?;
                Ok(image)
            })(),
            Err(error) => Err(error),
        };
        let release = self.leases.delete(lease.id());
        match (result, release) {
            (Ok(image), Ok(_)) => {
                hl_log::hl_info!(hl_log::tag::IMAGE, "pull complete name={}", image.name);
                Ok(image)
            }
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn pull_under_lease(
        &self,
        source: &(impl Source + ?Sized),
        reference: &Reference,
        platform: &Platform,
        lease: &str,
    ) -> Result<Descriptor> {
        let root = source.resolve(reference).await?;
        let root_bytes = self.fetch(source, reference, &root, lease, true).await?;
        let manifest = if root.is_index() {
            let index: IndexDocument = serde_json::from_slice(&root_bytes)?;
            index.select_platform(platform)?
        } else if root.is_manifest() {
            root.clone()
        } else {
            return Err(Error::MalformedOci(format!(
                "unsupported root media type {}",
                root.media_type()
            )));
        };
        let manifest_bytes = if manifest.digest() == root.digest() {
            root_bytes
        } else {
            self.fetch(source, reference, &manifest, lease, true)
                .await?
        };
        let document: ManifestDocument = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| Error::MalformedOci(error.to_string()))?;
        document.validate()?;
        self.fetch(source, reference, &document.config, lease, false)
            .await?;
        for layer in &document.layers {
            self.fetch(source, reference, layer, lease, false).await?;
        }
        Ok(manifest)
    }

    async fn fetch(
        &self,
        source: &(impl Source + ?Sized),
        reference: &Reference,
        descriptor: &Descriptor,
        lease: &str,
        capture: bool,
    ) -> Result<Bytes> {
        let digest: Digest = descriptor.digest().to_string().parse()?;
        self.leases.add(lease, format!("content:{digest}"))?;
        if self.content.contains(&digest)? {
            return if capture {
                self.content.read_document(descriptor)
            } else {
                Ok(Bytes::new())
            };
        }
        let mut stream = source.fetch(reference, descriptor).await?;
        let mut ingest = self.content.ingest(format!("pull-{digest}"))?;
        if capture && descriptor.size() > 16 * 1024 * 1024 {
            return Err(Error::MalformedOci(
                "descriptor document exceeds 16 MiB".into(),
            ));
        }
        let mut bytes = if capture {
            Vec::with_capacity(usize::try_from(descriptor.size()).unwrap_or(0))
        } else {
            Vec::new()
        };
        let mut received = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or(Error::SizeMismatch {
                    expected: descriptor.size(),
                    actual: u64::MAX,
                })?;
            if received > descriptor.size() {
                return Err(Error::SizeMismatch {
                    expected: descriptor.size(),
                    actual: received,
                });
            }
            ingest.write(&chunk)?;
            if capture {
                bytes.extend_from_slice(&chunk);
            }
        }
        ingest.commit(descriptor)?;
        Ok(bytes.into())
    }

    /// Validate image config `DiffIDs` and apply ordered layers into an immutable snapshot chain.
    ///
    /// # Errors
    /// Returns an error for missing/corrupt content, invalid configuration, unsafe layers, or snapshot failures.
    pub fn unpack(&self, image: &Image, platform: &Platform) -> Result<UnpackedImage> {
        // Publishing an immutable chain and pinning it must be one operation with
        // respect to GC.  Without this lock GC can remove the newly committed,
        // not-yet-leased chain while rootfs() is about to fork it.
        let _operation = crate::storage::ExclusiveLock::acquire(&self.operation_lock)?;
        let root_bytes = self.content.read_document(&image.target)?;
        let manifest = if image.target.is_index() {
            let index: IndexDocument = serde_json::from_slice(&root_bytes)?;
            index.select_platform(platform)?
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
        if config.rootfs.kind != "layers" {
            return Err(Error::MalformedOci(format!(
                "unsupported rootfs type {}",
                config.rootfs.kind
            )));
        }
        if config.rootfs.diff_ids.len() != document.layers.len() {
            return Err(Error::MalformedOci(format!(
                "config has {} DiffIDs for {} layers",
                config.rootfs.diff_ids.len(),
                document.layers.len()
            )));
        }
        let runtime = RuntimeConfig::try_from(config.config)?;
        let mut chain = Vec::with_capacity(config.rootfs.diff_ids.len());
        let mut parent = None;
        for expected in &config.rootfs.diff_ids {
            let expected: Digest = expected.parse()?;
            let id = Id::chain(parent.as_ref(), &expected)?;
            chain.push((expected, id.clone()));
            parent = Some(id);
        }
        // OCI scratch images have an empty diff-id chain. They all share the same
        // immutable empty root, just as equal non-empty chains share a snapshot.
        let snapshot = parent.unwrap_or(Id::new("chain-empty")?);

        // Older stores may contain a directory and metadata sidecars left behind
        // by the former unpack/GC race, but no filesystem entries.  A non-empty
        // OCI diff chain cannot use that as a cache hit.  Remove the unusable
        // publication while holding the shared operation lock and reconstruct it.
        if !chain.is_empty()
            && self.snapshots.contains(&snapshot)
            && self.snapshots.is_empty(&snapshot)?
        {
            self.snapshots.remove(&snapshot)?;
        }

        if !self.snapshots.contains(&snapshot) {
            let cached = chain
                .iter()
                .rposition(|(_, id)| self.snapshots.contains(id));
            let first = cached.map_or(0, |index| index + 1);
            let mut active = self.snapshots.prepare(
                Id::new(format!("apply-{}", uuid::Uuid::new_v4()))?,
                cached.map(|index| &chain[index].1),
            )?;
            for (layer, (expected, _)) in document.layers[first..].iter().zip(&chain[first..]) {
                let path = active.path().to_owned();
                let (ownerships, names) = active.metadata_mut();
                let actual = match self.content.apply_layer(layer, &path, ownerships, names) {
                    Ok(actual) => actual,
                    Err(error) => {
                        let _ = active.abort();
                        return Err(error);
                    }
                };
                if actual != *expected {
                    let _ = active.abort();
                    return Err(Error::DiffIdMismatch {
                        expected: expected.to_string(),
                        actual: actual.to_string(),
                    });
                }
            }
            active.commit(snapshot.clone())?;
        }
        let lease = self.leases.create_with(
            BTreeMap::from([
                ("kind".into(), "unpacked-image".into()),
                ("snapshot".into(), snapshot.as_str().into()),
            ]),
            [format!("snapshot:{}", snapshot.as_str())],
        )?;
        Ok(UnpackedImage {
            image: image.clone(),
            manifest,
            snapshot,
            platform: platform.clone(),
            runtime,
            _lease: Arc::new(UnpackedLease {
                leases: self.leases.clone(),
                id: lease.id().into(),
            }),
        })
    }
}
