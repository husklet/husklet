use async_trait::async_trait;
use bytes::Bytes;
use flate2::{write::GzEncoder, Compression};
use futures_util::stream;
use hl_images::{remote::Source, Descriptor, Digest, Error, Reference};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Default)]
pub(super) struct FaultPersistence {
    metadata: AtomicUsize,
    blob: AtomicUsize,
}

impl FaultPersistence {
    pub(super) fn fail_metadata_in(&self, operations: usize) {
        self.metadata.store(operations, Ordering::SeqCst);
    }

    pub(super) fn fail_blob_in(&self, operations: usize) {
        self.blob.store(operations, Ordering::SeqCst);
    }

    fn fails(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                if value > 0 {
                    Some(value - 1)
                } else {
                    None
                }
            })
            .is_ok_and(|previous| previous == 1)
    }
}

impl hl_images::storage::Persistence for FaultPersistence {
    fn replace(&self, path: &Path, bytes: &[u8]) -> hl_images::Result<()> {
        if path.ends_with("images.json") && Self::fails(&self.metadata) {
            return Err(std::io::Error::other("injected metadata replacement failure").into());
        }
        hl_images::storage::Persistence::replace(&hl_images::storage::Native, path, bytes)
    }

    fn remove(&self, path: &Path) -> hl_images::Result<bool> {
        if path.components().any(|part| part.as_os_str() == "blobs") && Self::fails(&self.blob) {
            return Err(std::io::Error::other("injected blob removal failure").into());
        }
        hl_images::storage::Persistence::remove(&hl_images::storage::Native, path)
    }
}

#[derive(Clone)]
pub(super) struct MemorySource {
    pub(super) root: Descriptor,
    pub(super) blobs: Arc<HashMap<String, Bytes>>,
}

pub(super) enum Broken {
    Oversized,
    Truncated,
    Corrupt,
}
pub(super) struct BrokenSource {
    pub(super) root: Descriptor,
    pub(super) bytes: Bytes,
    pub(super) kind: Broken,
}
#[async_trait]
impl Source for BrokenSource {
    async fn resolve(&self, _: &Reference) -> hl_images::Result<Descriptor> {
        Ok(self.root.clone())
    }
    async fn fetch(
        &self,
        _: &Reference,
        _: &Descriptor,
    ) -> hl_images::Result<hl_images::remote::BlobStream> {
        let bytes = match self.kind {
            Broken::Oversized => {
                let mut value = self.bytes.to_vec();
                value.push(0);
                Bytes::from(value)
            }
            Broken::Truncated => self.bytes.slice(..self.bytes.len() - 1),
            Broken::Corrupt => {
                let mut value = self.bytes.to_vec();
                value[0] ^= 0xff;
                Bytes::from(value)
            }
        };
        Ok(Box::pin(stream::once(async move { Ok(bytes) })))
    }
}
#[async_trait]
impl Source for MemorySource {
    async fn resolve(&self, _: &Reference) -> hl_images::Result<Descriptor> {
        Ok(self.root.clone())
    }
    async fn fetch(
        &self,
        _: &Reference,
        descriptor: &Descriptor,
    ) -> hl_images::Result<hl_images::remote::BlobStream> {
        let bytes = self
            .blobs
            .get(&descriptor.digest().to_string())
            .cloned()
            .ok_or_else(|| Error::Registry(format!("missing {}", descriptor.digest())))?;
        let chunks = bytes
            .chunks(3)
            .map(Bytes::copy_from_slice)
            .map(Ok)
            .collect::<Vec<_>>();
        Ok(Box::pin(stream::iter(chunks)))
    }
}

pub(super) fn descriptor(media: &str, bytes: &[u8]) -> Descriptor {
    serde_json::from_value(serde_json::json!({"mediaType":media,"digest":Digest::sha256(bytes).to_string(),"size":bytes.len()})).unwrap()
}
pub(super) fn tar_file(path: &str, content: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, content).unwrap();
        builder.finish().unwrap();
    }
    bytes
}

pub(super) fn docker_save_archive(tag: &str, os: &str, labels: &serde_json::Value) -> Vec<u8> {
    let layer = tar_file("etc/release", b"archive\n");
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": os,
        "config": {"Cmd": ["/bin/true"], "Labels": labels},
        "rootfs": {"type": "layers", "diff_ids": [Digest::sha256(&layer).to_string()]}
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json", "RepoTags": [tag], "Layers": ["layer.tar"]
    }]))
    .unwrap();
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        for (name, contents) in [
            ("manifest.json", manifest.as_slice()),
            ("config.json", config.as_slice()),
            ("layer.tar", layer.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, contents).unwrap();
        }
        builder.finish().unwrap();
    }
    bytes
}

pub(super) fn fixture(diff_override: Option<String>) -> (MemorySource, Descriptor) {
    let base_tar = tar_file("etc/base", b"base\n");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&base_tar).unwrap();
    let base_bytes = encoder.finish().unwrap();
    let base = descriptor("application/vnd.oci.image.layer.v1.tar+gzip", &base_bytes);
    let layer_tar = tar_file("etc/release", b"husklet\n");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&layer_tar).unwrap();
    let layer_bytes = encoder.finish().unwrap();
    let layer = descriptor("application/vnd.oci.image.layer.v1.tar+gzip", &layer_bytes);
    let diff = diff_override.unwrap_or_else(|| Digest::sha256(&layer_tar).to_string());
    let config_bytes = serde_json::to_vec(&serde_json::json!({"architecture":"arm64","os":"linux","config":{"Entrypoint":["/bin/app"],"Cmd":["serve"],"Env":["A=old","B=two","A=new"],"WorkingDir":"/work","User":"1000:1000"},"rootfs":{"type":"layers","diff_ids":[Digest::sha256(&base_tar).to_string(),diff]}})).unwrap();
    let config = descriptor("application/vnd.oci.image.config.v1+json", &config_bytes);
    let manifest_bytes = serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":config,"layers":[base,layer]})).unwrap();
    let manifest = descriptor(
        "application/vnd.oci.image.manifest.v1+json",
        &manifest_bytes,
    );
    let index_bytes = serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[merge_descriptor(&manifest, serde_json::json!({"os":"linux","architecture":"arm64","variant":"v8"}))]})).unwrap();
    let index = descriptor("application/vnd.oci.image.index.v1+json", &index_bytes);
    let blobs = HashMap::from([
        (index.digest().to_string(), Bytes::from(index_bytes)),
        (manifest.digest().to_string(), Bytes::from(manifest_bytes)),
        (config.digest().to_string(), Bytes::from(config_bytes)),
        (base.digest().to_string(), Bytes::from(base_bytes)),
        (layer.digest().to_string(), Bytes::from(layer_bytes)),
    ]);
    (
        MemorySource {
            root: index.clone(),
            blobs: Arc::new(blobs),
        },
        manifest,
    )
}

pub(super) fn scratch_fixture() -> MemorySource {
    let config_bytes = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {"Cmd": ["/bin/true"]},
        "rootfs": {"type": "layers", "diff_ids": []}
    }))
    .unwrap();
    let config = descriptor("application/vnd.oci.image.config.v1+json", &config_bytes);
    let manifest_bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": config,
        "layers": []
    }))
    .unwrap();
    let manifest = descriptor(
        "application/vnd.oci.image.manifest.v1+json",
        &manifest_bytes,
    );
    let index_bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [merge_descriptor(&manifest, serde_json::json!({
            "os": "linux", "architecture": "arm64"
        }))]
    }))
    .unwrap();
    let index = descriptor("application/vnd.oci.image.index.v1+json", &index_bytes);
    MemorySource {
        root: index.clone(),
        blobs: Arc::new(HashMap::from([
            (index.digest().to_string(), Bytes::from(index_bytes)),
            (manifest.digest().to_string(), Bytes::from(manifest_bytes)),
            (config.digest().to_string(), Bytes::from(config_bytes)),
        ])),
    }
}

pub(super) fn invalid_config_fixture() -> MemorySource {
    let config_bytes = b"{invalid config".to_vec();
    let config = descriptor("application/vnd.oci.image.config.v1+json", &config_bytes);
    let manifest_bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": config,
        "layers": []
    }))
    .unwrap();
    let manifest = descriptor(
        "application/vnd.oci.image.manifest.v1+json",
        &manifest_bytes,
    );
    let index_bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [merge_descriptor(&manifest, serde_json::json!({
            "os": "linux", "architecture": "arm64"
        }))]
    }))
    .unwrap();
    let index = descriptor("application/vnd.oci.image.index.v1+json", &index_bytes);
    MemorySource {
        root: index.clone(),
        blobs: Arc::new(HashMap::from([
            (index.digest().to_string(), Bytes::from(index_bytes)),
            (manifest.digest().to_string(), Bytes::from(manifest_bytes)),
            (config.digest().to_string(), Bytes::from(config_bytes)),
        ])),
    }
}
pub(super) fn merge_descriptor(
    descriptor: &Descriptor,
    platform: serde_json::Value,
) -> serde_json::Value {
    let mut value = serde_json::to_value(descriptor).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("platform".into(), platform);
    value
}
