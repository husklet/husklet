pub(super) use std::collections::{BTreeMap, HashMap};
pub(super) use std::time::Duration;

pub(super) use async_trait::async_trait;
pub(super) use bytes::Bytes;
pub(super) use futures_util::stream;
pub(super) use std::io::Read as _;

pub(super) use hl_client::{
    Client,
    api::WaitCondition,
    model::{EventFilter, EventQuery, NetworkConnect, NetworkCreate, NetworkDisconnect, VolumeCreate},
};
pub(super) use hl_container::{Config, ContainerSpec, Containers, Mount, Persistence, Process};
pub(super) use hl_daemon::{Daemon, Error};
pub(super) use hl_images::{
    Descriptor, Digest, LeaseStore, Platform, Reference,
    format::docker::{Archive, Limits},
    remote::{BlobStream, Source},
};
pub(super) use tempfile::TempDir;
pub(super) use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub(super) use tokio::net::UnixStream;
pub(super) use tokio::sync::oneshot;

pub(super) async fn containers(root: &TempDir) -> Containers {
    Containers::builder(Config::new(root.path()).persistence(Persistence::Memory))
        .build()
        .await
        .unwrap()
}

pub(super) struct TestDaemon {
    pub(super) client: Client,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), Error>>,
}

impl TestDaemon {
    pub(super) async fn start(containers: Containers, socket: &std::path::Path) -> Self {
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(Daemon::new(containers).server(socket).serve_with_shutdown(async move {
            let _ = stopped.await;
        }));
        wait_for_socket(socket).await;
        Self {
            client: Client::unix(socket).unwrap(),
            stop,
            task,
        }
    }

    pub(super) async fn stop(self) {
        self.stop.send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

pub(super) async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("server did not become ready");
}

pub(super) async fn raw_http(path: &std::path::Path, request: &[u8]) -> String {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(request).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8(response).unwrap()
}

fn append(builder: &mut tar::Builder<&mut Vec<u8>>, path: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

pub(super) fn docker_archive() -> Vec<u8> {
    let mut layer = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut layer);
        append(&mut tar, "etc/release", b"containers-test\n");
        append(&mut tar, "anonymous/seed", b"from-image");
        tar.finish().unwrap();
    }
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "created": "2026-07-15T12:34:56Z",
        "history": [{
            "created": "2026-07-15T12:34:56Z",
            "created_by": "/bin/sh -c #(nop) ADD fixture",
            "comment": "integration fixture"
        }],
        "config": {
            "Entrypoint": ["/bin/echo"],
            "Cmd": ["from-image"],
            "Env": ["IMAGE=yes"],
            "WorkingDir": "/work",
            "User": "0:0"
        },
        "rootfs": {"type": "layers", "diff_ids": [Digest::sha256(&layer).to_string()]}
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": ["scenario/fixture:v1"],
        "Layers": ["layer/layer.tar"]
    }]))
    .unwrap();
    let mut archive = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut archive);
        append(&mut tar, "config.json", &config);
        append(&mut tar, "layer/layer.tar", &layer);
        append(&mut tar, "manifest.json", &manifest);
        tar.finish().unwrap();
    }
    archive
}

pub(super) fn dockerfile_context(source: &[u8]) -> Vec<u8> {
    let mut context = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut context);
        append(&mut archive, "Dockerfile", source);
        archive.finish().unwrap();
    }
    context
}

pub(super) fn runnable_archive() -> Vec<u8> {
    let source = std::env::var_os("HL_ALPINE_ARCHIVE")
        .map(std::path::PathBuf::from)
        .expect("HL_ALPINE_ARCHIVE must name the pinned Alpine minirootfs");
    let compressed = std::fs::read(&source).unwrap();
    let mut layer = Vec::new();
    flate2::read::GzDecoder::new(&compressed[..])
        .read_to_end(&mut layer)
        .unwrap();
    let architecture = if source.to_string_lossy().contains("x86_64") {
        "amd64"
    } else {
        "arm64"
    };
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": architecture,
        "os": "linux",
        "config": {"Cmd": ["/bin/sh"], "WorkingDir": "/"},
        "rootfs": {
            "type": "layers",
            "diff_ids": [Digest::sha256(&layer).to_string()]
        }
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": "config.json",
        "RepoTags": ["scenario/runnable:v1"],
        "Layers": ["layer.tar"]
    }]))
    .unwrap();
    let mut archive = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut archive);
        append(&mut tar, "config.json", &config);
        append(&mut tar, "layer.tar", &layer);
        append(&mut tar, "manifest.json", &manifest);
        tar.finish().unwrap();
    }
    archive
}

#[derive(Clone)]
pub(super) struct FixtureSource {
    root: Descriptor,
    blobs: HashMap<String, Bytes>,
}

#[async_trait]
impl Source for FixtureSource {
    async fn resolve(&self, _reference: &Reference) -> hl_images::Result<Descriptor> {
        Ok(self.root.clone())
    }

    async fn fetch(&self, _reference: &Reference, descriptor: &Descriptor) -> hl_images::Result<BlobStream> {
        let bytes = self
            .blobs
            .get(&descriptor.digest().to_string())
            .cloned()
            .ok_or_else(|| hl_images::Error::ContentNotFound(descriptor.digest().to_string()))?;
        Ok(Box::pin(stream::once(async move { Ok(bytes) })))
    }
}

fn descriptor(media_type: &str, bytes: &[u8]) -> Descriptor {
    serde_json::from_value(serde_json::json!({
        "mediaType": media_type,
        "digest": Digest::sha256(bytes).to_string(),
        "size": bytes.len()
    }))
    .unwrap()
}

pub(super) fn registry_fixture() -> FixtureSource {
    let layer = b"offline-layer".to_vec();
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": "arm64",
        "os": "linux",
        "config": {},
        "rootfs": {"type": "layers", "diff_ids": [Digest::sha256(&layer).to_string()]}
    }))
    .unwrap();
    let config_descriptor = descriptor("application/vnd.oci.image.config.v1+json", &config);
    let layer_descriptor = descriptor("application/vnd.oci.image.layer.v1.tar", &layer);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": config_descriptor,
        "layers": [layer_descriptor]
    }))
    .unwrap();
    let root = descriptor("application/vnd.oci.image.manifest.v1+json", &manifest);
    let blobs = [
        (root.digest().to_string(), Bytes::from(manifest)),
        (config_descriptor.digest().to_string(), Bytes::from(config)),
        (layer_descriptor.digest().to_string(), Bytes::from(layer)),
    ]
    .into_iter()
    .collect();
    FixtureSource { root, blobs }
}

pub(super) async fn basic_registry(
    username: &'static str,
    password: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    use base64::Engine as _;
    let fixture = registry_fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    );
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let root = fixture.root.clone();
            let blobs = fixture.blobs.clone();
            let expected = expected.clone();
            tokio::spawn(async move {
                let mut request = vec![0; 16 * 1024];
                let count = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap();
                if !request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case(&format!("authorization: {expected}")))
                {
                    socket
                        .write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"mock\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                    return;
                }
                let digest = path.rsplit('/').next().unwrap();
                let body = if path.contains("/manifests/") {
                    blobs[&root.digest().to_string()].clone()
                } else {
                    blobs[digest].clone()
                };
                let digest_header = if path.contains("/manifests/") {
                    format!("Docker-Content-Digest: {}\r\n", root.digest())
                } else {
                    String::new()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n{digest_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    if path.contains("/manifests/") {
                        root.media_type().to_string()
                    } else {
                        "application/octet-stream".into()
                    },
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            });
        }
    });
    (format!("{address}/team/demo"), task)
}

pub(super) fn docker_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        for (path, contents) in entries {
            append(&mut archive, path, contents);
        }
        archive.finish().unwrap();
    }
    bytes
}
