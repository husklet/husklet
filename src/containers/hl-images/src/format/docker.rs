use sha2::Digest as _;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, Write},
    path::{Component, Path},
};

use crate::{Descriptor, Digest, Error, Image, Images, Result, content::Store};

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const OCI_LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
const OCI_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const OCI_LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_metadata_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 128 * 1024 * 1024 * 1024,
            max_metadata_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Docker save archive compatibility at the OCI storage edge.
pub struct Archive;
impl Archive {
    /// Import regular archive members, translating Docker metadata into OCI manifests.
    ///
    /// # Errors
    /// Returns an error for unsafe members, exceeded bounds, malformed metadata, or integrity/storage failures.
    pub fn load(reader: impl Read, images: &Images, limits: Limits) -> Result<Vec<Image>> {
        let staging = tempfile::tempdir()?;
        let mut members = HashMap::new();
        let mut count = 0_u64;
        let mut total = 0_u64;
        let mut archive = tar::Archive::new(reader);
        let entries = archive.entries().map_err(Self::error)?;
        for item in entries {
            let mut entry = item.map_err(Self::error)?;
            count += 1;
            if count > limits.max_entries {
                return Err(Error::MalformedOci("archive entry limit exceeded".into()));
            }
            let path = Self::member(&entry.path().map_err(Self::error)?)?;
            if entry.header().entry_type().is_dir() {
                continue;
            }
            if !entry.header().entry_type().is_file() {
                return Err(Error::UnsafeArchive {
                    path,
                    reason: "Docker archive members must be regular files",
                });
            }
            let size = entry.size();
            total = total
                .checked_add(size)
                .ok_or_else(|| Error::MalformedOci("archive size overflow".into()))?;
            if total > limits.max_total_bytes {
                return Err(Error::MalformedOci("archive byte limit exceeded".into()));
            }
            let name = path.to_string_lossy().into_owned();
            if members.contains_key(&name) {
                return Err(Error::MalformedOci(format!("duplicate archive member {name}")));
            }
            let destination = staging.path().join(format!("member-{count}"));
            let mut file = File::create(&destination)?;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = entry.read(&mut buffer).map_err(Self::error)?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read])?;
            }
            file.sync_all()?;
            members.insert(name, (destination, size));
        }
        let manifest = Self::metadata(&members, "manifest.json", limits.max_metadata_bytes)?;
        let records: Vec<DockerManifest> =
            serde_json::from_slice(&manifest).map_err(|error| Error::MalformedOci(error.to_string()))?;
        let mut imported = Vec::new();
        for record in records {
            if record.repo_tags.is_empty() {
                return Err(Error::MalformedOci("Docker manifest entry has no RepoTags".into()));
            }
            let config_bytes = Self::metadata(&members, &record.config, limits.max_metadata_bytes)?;
            let config = images.store_bytes(OCI_CONFIG, &config_bytes)?;
            let mut layers = Vec::with_capacity(record.layers.len());
            for name in &record.layers {
                let (path, _) = members
                    .get(name)
                    .ok_or_else(|| Error::MalformedOci(format!("missing layer member {name}")))?;
                layers.push(images.store_file(Self::layer_media(path)?, path)?);
            }
            let manifest_bytes = serde_json::to_vec(
                &serde_json::json!({"schemaVersion":2,"mediaType":OCI_MANIFEST,"config":config,"layers":layers}),
            )?;
            let target = images.store_bytes(OCI_MANIFEST, &manifest_bytes)?;
            for tag in record.repo_tags {
                let image = Image {
                    name: tag.parse()?,
                    target: target.clone(),
                };
                imported.push(image);
            }
        }
        images.metadata().put_all(imported.clone())?;
        Ok(imported)
    }

    /// Export selected images as a deterministic Docker save archive.
    ///
    /// # Errors
    /// Returns an error for missing/corrupt OCI content or archive write failures.
    pub fn save(writer: impl Write, images: &Images, selected: &[Image]) -> Result<()> {
        let mut tar = tar::Builder::new(writer);
        let mut manifest = Vec::new();
        let mut written = HashSet::new();
        {
            let mut archive = DockerWriter {
                tar: &mut tar,
                images,
                written: &mut written,
            };
            for image in selected {
                let document: OciManifest = serde_json::from_reader(images.content().reader(&image.target)?)?;
                let config_digest: Digest = document.config.digest().to_string().parse()?;
                let config_name = format!("{}.json", config_digest.encoded());
                archive.content(&document.config, &config_name)?;
                let mut layer_names = Vec::new();
                for layer in &document.layers {
                    let digest: Digest = layer.digest().to_string().parse()?;
                    let name = format!("{}/layer.tar", digest.encoded());
                    archive.layer(layer, &name)?;
                    layer_names.push(name);
                }
                manifest.push(
                    serde_json::json!({"Config":config_name,"RepoTags":[image.name.to_string()],"Layers":layer_names}),
                );
            }
            let bytes = serde_json::to_vec(&manifest)?;
            archive.bytes("manifest.json", &bytes)?;
        }
        tar.finish()?;
        Ok(())
    }

    fn member(path: &Path) -> Result<std::path::PathBuf> {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path.components().any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(Error::UnsafeArchive {
                path: path.to_owned(),
                reason: "archive path is not normalized and relative",
            });
        }
        Ok(path.to_owned())
    }

    fn metadata(members: &HashMap<String, (std::path::PathBuf, u64)>, name: &str, limit: u64) -> Result<Vec<u8>> {
        let (path, size) = members
            .get(name)
            .ok_or_else(|| Error::MalformedOci(format!("missing archive member {name}")))?;
        if *size > limit {
            return Err(Error::MalformedOci(format!("metadata member {name} exceeds limit")));
        }
        std::fs::read(path).map_err(Into::into)
    }

    fn layer_media(path: &Path) -> Result<&'static str> {
        let mut file = File::open(path)?;
        let mut magic = [0_u8; 4];
        let count = file.read(&mut magic)?;
        Ok(if count >= 2 && magic[..2] == [0x1f, 0x8b] {
            OCI_LAYER_GZIP
        } else if count == 4 && magic == [0x28, 0xb5, 0x2f, 0xfd] {
            OCI_LAYER_ZSTD
        } else {
            OCI_LAYER
        })
    }

    fn error(error: std::io::Error) -> Error {
        let message = error.to_string();
        drop(error);
        Error::MalformedOci(format!("invalid Docker archive: {message}"))
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerManifest {
    config: String,
    #[serde(default)]
    repo_tags: Vec<String>,
    layers: Vec<String>,
}
#[derive(serde::Deserialize)]
struct OciManifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

impl Images {
    fn store_bytes(&self, media: &str, bytes: &[u8]) -> Result<Descriptor> {
        let descriptor: Descriptor = serde_json::from_value(serde_json::json!({
            "mediaType": media,
            "digest": Digest::sha256(bytes).to_string(),
            "size": bytes.len()
        }))?;
        let digest: Digest = descriptor.digest().to_string().parse()?;
        if !self.content().contains(&digest)? {
            let mut ingest = self.content().ingest(format!("archive-{digest}"))?;
            ingest.write(bytes)?;
            ingest.commit(&descriptor)?;
        }
        Ok(descriptor)
    }
    fn store_file(&self, media: &str, path: &Path) -> Result<Descriptor> {
        let mut file = File::open(path)?;
        let mut hash = sha2::Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            size += count as u64;
        }
        let digest = Digest::from(<[u8; 32]>::from(hash.finalize()));
        let descriptor: Descriptor =
            serde_json::from_value(serde_json::json!({"mediaType":media,"digest":digest.to_string(),"size":size}))?;
        if !self.content().contains(&digest)? {
            let mut ingest = self.content().ingest(format!("archive-{digest}"))?;
            let mut file = File::open(path)?;
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                ingest.write(&buffer[..count])?;
            }
            ingest.commit(&descriptor)?;
        }
        Ok(descriptor)
    }
}
struct DockerWriter<'a, W: Write> {
    tar: &'a mut tar::Builder<W>,
    images: &'a Images,
    written: &'a mut HashSet<String>,
}

impl<W: Write> DockerWriter<'_, W> {
    fn content(&mut self, descriptor: &Descriptor, name: &str) -> Result<()> {
        if self.written.insert(name.into()) {
            let mut header = tar::Header::new_gnu();
            header.set_size(descriptor.size());
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            self.tar
                .append_data(&mut header, name, self.images.content().reader(descriptor)?)?;
        }
        Ok(())
    }

    fn layer(&mut self, descriptor: &Descriptor, name: &str) -> Result<()> {
        if !self.written.insert(name.into()) {
            return Ok(());
        }
        let media = descriptor.media_type().to_string();
        if media.ends_with("+gzip") || media.rsplit('.').next() == Some("gzip") {
            let mut decoded = tempfile::tempfile()?;
            std::io::copy(
                &mut flate2::read::GzDecoder::new(self.images.content().reader(descriptor)?),
                &mut decoded,
            )?;
            let size = decoded.metadata()?.len();
            decoded.rewind()?;
            let mut header = tar::Header::new_gnu();
            header.set_size(size);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            self.tar.append_data(&mut header, name, decoded)?;
            Ok(())
        } else if media.ends_with("+zstd") {
            Err(Error::MalformedOci("cannot export zstd layer to Docker archive".into()))
        } else {
            let mut header = tar::Header::new_gnu();
            header.set_size(descriptor.size());
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            self.tar
                .append_data(&mut header, name, self.images.content().reader(descriptor)?)?;
            Ok(())
        }
    }

    fn bytes(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        self.tar.append_data(&mut header, name, bytes)?;
        Ok(())
    }
}
