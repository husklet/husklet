//! Turning a container image into something a person can be asked to consent to.
//!
//! An extension declares itself in a TOML document inside its image, named by
//! the [`Manifest::LABEL`] image label. Reading it needs a container daemon, so
//! this module is split: the daemon walk is one function, and everything it
//! decides — which path to read, which entry of the returned archive is the
//! document — is a pure function tested with no image and no daemon.
//!
//! Nothing here records anything. It produces a [`Candidate`], which is what a
//! consent prompt is drawn from; writing the grant is [`Roster::register`]'s,
//! and only ever with an answer a person gave.
//!
//! [`Roster::register`]: super::Roster::register

use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::mpsc::Sender;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use hl_client::model::{CreateContainer, InspectImage};
use hl_extension::port::HostError;
use hl_extension::{Manifest, PROTOCOL};

use super::Bridge;
use crate::config::WorkspaceConfig;

/// Largest archive read while looking for a manifest.
///
/// The daemon hands back a tar stream, and a tar entry's own header is not
/// trusted to be honest about its size, so the read is bounded here instead.
/// Four times the manifest limit leaves room for the archive framing around a
/// document at the limit and refuses anything that is not a manifest at all.
pub const ARCHIVE_LIMIT: usize = 4 * Manifest::LIMIT;
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(25);
const CLEANUP_BOUND: std::time::Duration = std::time::Duration::from_secs(2);

/// Cooperative authority to stop one image acquisition.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("cancelled".to_owned())
        } else {
            Ok(())
        }
    }
}

/// An image inspected far enough to ask a person about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    /// The reference that was inspected, as it was typed.
    pub reference: String,
    /// The digest a grant would be tied to.
    pub digest: String,
    /// What the image declares, and therefore what is being asked for.
    pub manifest: Manifest,
}

/// One truthful step while turning an image reference into a consent candidate.
///
/// The daemon currently reports registry status records, and may report byte
/// totals when its registry implementation has them. An absent total stays
/// absent: the UI must not turn a stage boundary into invented download
/// progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Acquisition {
    Inspecting,
    Pulling {
        status: String,
        id: Option<String>,
        current: Option<u64>,
        total: Option<u64>,
    },
    ReadingManifest,
    Ready(Candidate),
    Failed(String),
    Cancelled,
}

impl Candidate {
    /// Resolves a local image, pulling an absent reference before inspecting it.
    ///
    /// Every stage is sent to `progress`. Losing the receiver is not a failure:
    /// closing the window stops displaying an acquisition but must not panic a
    /// worker that is already inside the daemon.
    pub fn acquire(workspace: &WorkspaceConfig, reference: &str, progress: &Sender<Acquisition>) {
        Self::acquire_cancellable(workspace, reference, progress, &Cancellation::default());
    }

    pub fn acquire_cancellable(
        workspace: &WorkspaceConfig,
        reference: &str,
        progress: &Sender<Acquisition>,
        cancellation: &Cancellation,
    ) {
        let result = Self::acquire_inner(workspace, reference, progress, cancellation);
        let event = match result {
            Ok(candidate) => Acquisition::Ready(candidate),
            Err(_) if cancellation.is_cancelled() => Acquisition::Cancelled,
            Err(reason) => Acquisition::Failed(reason),
        };
        let _ = progress.send(event);
    }

    /// Reads what `reference` declares about itself.
    ///
    /// The image is inspected, a container is created from it but never
    /// started, its manifest is copied out, and the container is removed again.
    /// Creating without starting is what makes this safe to do to an image
    /// nobody has consented to yet: nothing in it runs.
    ///
    /// # Errors
    /// Returns why the workspace daemon could not be reached, the image could
    /// not be inspected, the manifest could not be read out of it, or the
    /// document is not a manifest this host speaks.
    pub fn read(workspace: &WorkspaceConfig, reference: &str) -> Result<Self, String> {
        let (sent, _ignored) = std::sync::mpsc::channel();
        Self::acquire_inner(workspace, reference, &sent, &Cancellation::default())
    }

    fn acquire_inner(
        workspace: &WorkspaceConfig,
        reference: &str,
        progress: &Sender<Acquisition>,
        cancellation: &Cancellation,
    ) -> Result<Self, String> {
        cancellation.check()?;
        let socket = crate::runtime::domain::Domain::new(workspace)
            .ensure(workspace)
            .map_err(|error| error.to_string())?;
        let bridge = Bridge::new(socket).map_err(|error| error.to_string())?;
        Self::acquire_with_bridge(reference, workspace.arch.as_str(), progress, cancellation, &bridge)
    }

    fn acquire_with_bridge(
        reference: &str,
        architecture: &str,
        progress: &Sender<Acquisition>,
        cancellation: &Cancellation,
        bridge: &Bridge,
    ) -> Result<Self, String> {
        let client = bridge.client();
        let _ = progress.send(Acquisition::Inspecting);
        cancellation.check()?;
        let inspection: InspectImage = match cancellable(&bridge, cancellation, client.images().inspect(reference))? {
            Ok(inspection) => inspection,
            Err(error) if matches!(super::failure(&error), HostError::Absent(_)) => {
                pull(bridge, reference, architecture, progress, cancellation)?;
                cancellable(&bridge, cancellation, client.images().inspect(reference))?
                    .map_err(|error| error.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        cancellation.check()?;
        platform(&inspection, architecture)?;
        let _ = progress.send(Acquisition::ReadingManifest);
        let path = manifest_path(&inspection.config.labels);
        let content = immutable_content(reference, &inspection.id)?;
        let archive = extract(&bridge, content, &path, cancellation)?;
        cancellation.check()?;
        let manifest = Manifest::parse(&document(&archive)?, PROTOCOL).map_err(|invalid| invalid.to_string())?;
        Ok(Self {
            reference: reference.to_owned(),
            digest: inspection.id,
            manifest,
        })
    }

    #[cfg(any(test, feature = "native-test-hooks"))]
    #[doc(hidden)]
    pub fn acquire_from_socket(
        socket: &std::path::Path,
        architecture: hl_ws::Arch,
        reference: &str,
        progress: &Sender<Acquisition>,
    ) {
        let result = Bridge::new(socket.to_path_buf())
            .map_err(|error| error.to_string())
            .and_then(|bridge| {
                Self::acquire_with_bridge(
                    reference,
                    architecture.as_str(),
                    progress,
                    &Cancellation::default(),
                    &bridge,
                )
            });
        let event = result.map_or_else(Acquisition::Failed, Acquisition::Ready);
        let _ = progress.send(event);
    }
}

/// Pins manifest extraction to the image identity that will be persisted.
/// Docker Hub tags may move after inspection; content digests do not.
fn immutable_content<'a>(_reference: &str, digest: &'a str) -> Result<&'a str, String> {
    (!digest.trim().is_empty())
        .then_some(digest)
        .ok_or_else(|| "the inspected extension image has no immutable digest".to_owned())
}

fn platform(inspection: &InspectImage, architecture: &str) -> Result<(), String> {
    if inspection.os != "linux" || inspection.architecture != architecture {
        return Err(format!(
            "extension image {} is {}/{}, but this workspace requires linux/{architecture}",
            inspection.id, inspection.os, inspection.architecture
        ));
    }
    Ok(())
}

/// Pulls one registry reference, forwarding only progress the daemon actually
/// supplied. The daemon embeds registry failures in an otherwise successful
/// HTTP stream, so every record must be inspected.
fn pull(
    bridge: &Bridge,
    reference: &str,
    architecture: &str,
    progress: &Sender<Acquisition>,
    cancellation: &Cancellation,
) -> Result<(), String> {
    let (name, tag) = split(reference);
    let client = bridge.client();
    let platform = format!("linux/{architecture}");
    let mut stream = cancellable(bridge, cancellation, client.images().pull(name, tag, Some(&platform)))?
        .map_err(|error| error.to_string())?;
    loop {
        let record = cancellable(bridge, cancellation, stream.next())?.map_err(|error| error.to_string())?;
        let Some(record) = record else { return Ok(()) };
        if let Some(reason) = record.error {
            return Err(reason);
        }
        let detail = record.progress_detail;
        let _ = progress.send(Acquisition::Pulling {
            status: record.status.unwrap_or_else(|| "pulling image".to_owned()),
            id: record.id,
            current: detail.as_ref().and_then(|value| u64::try_from(value.current).ok()),
            total: detail.and_then(|value| u64::try_from(value.total).ok()),
        });
    }
}

/// Splits a reference without mistaking a registry host's port for a tag.
fn split(reference: &str) -> (&str, Option<&str>) {
    if let Some(index) = reference.find('@') {
        return (&reference[..index], Some(&reference[index + 1..]));
    }
    let start = reference.rfind('/').map_or(0, |index| index + 1);
    let Some(relative) = reference[start..].rfind(':') else {
        return (reference, None);
    };
    let index = start + relative;
    (&reference[..index], Some(&reference[index + 1..]))
}

/// Copies one path out of a container made from `reference`, and removes it again.
fn extract(bridge: &Bridge, reference: &str, path: &str, cancellation: &Cancellation) -> Result<Vec<u8>, String> {
    let client = bridge.client();
    let request = CreateContainer {
        image: reference.to_owned(),
        ..CreateContainer::default()
    };
    let created = cancellable(bridge, cancellation, client.containers().create(&request, None))?
        .map_err(|error| error.to_string())?;
    let archive = cancellable(bridge, cancellation, read(bridge, &created.id, path))?;
    // The container is removed whatever the read did: one left behind for every
    // image a person looked at and did not install is a leak nobody would
    // connect to this screen.
    let _ = bridge
        .wait(async { tokio::time::timeout(CLEANUP_BOUND, client.containers().remove(&created.id, true, true)).await });
    archive
}

fn cancellable<F: std::future::Future>(
    bridge: &Bridge,
    cancellation: &Cancellation,
    work: F,
) -> Result<F::Output, String> {
    bridge.wait(async {
        tokio::pin!(work);
        loop {
            tokio::select! {
                answer = &mut work => return Ok(answer),
                () = tokio::time::sleep(CANCEL_POLL) => cancellation.check()?,
            }
        }
    })
}

/// Streams the archive the daemon returns, bounded by [`ARCHIVE_LIMIT`].
async fn read(bridge: &Bridge, container: &str, path: &str) -> Result<Vec<u8>, String> {
    let client = bridge.client();
    let archive = client
        .containers()
        .copy_from(container, path)
        .await
        .map_err(|error| format!("{path} could not be read from the image: {error}"))?;
    let mut stream = archive.into_stream();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next_chunk().await.map_err(|error| error.to_string())? {
        if collected.len() + chunk.len() > ARCHIVE_LIMIT {
            return Err(format!("the manifest at {path} is larger than {ARCHIVE_LIMIT} bytes"));
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected)
}

/// Where the manifest lives inside an image, as its labels say.
///
/// A label that is present but blank falls back to the default rather than
/// reading the container root, because an empty path is a build mistake and the
/// default is what the author meant.
#[must_use]
pub fn manifest_path(labels: &BTreeMap<String, String>) -> String {
    labels
        .get(Manifest::LABEL)
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .unwrap_or(Manifest::DEFAULT_PATH)
        .to_owned()
}

/// The document inside the archive the daemon returned.
///
/// Docker answers a file copy with a tar holding that one file, so the first
/// regular entry is the manifest. Directories are skipped rather than refused,
/// since a copy of a path can carry the entry for its own parent.
///
/// # Errors
/// Returns why the archive held no readable document.
pub fn document(archive: &[u8]) -> Result<String, String> {
    let mut reader = tar::Archive::new(archive);
    let entries = reader
        .entries()
        .map_err(|error| format!("the image's manifest archive is unreadable: {error}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("the image's manifest archive is unreadable: {error}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let mut document = String::new();
        return match entry.read_to_string(&mut document) {
            Ok(_) => Ok(document),
            Err(error) => Err(format!("the image's manifest is not text: {error}")),
        };
    }
    Err("the image carries no manifest at the path its label names".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{document, immutable_content, manifest_path, split, Acquisition, Candidate};
    use hl_extension::Manifest;
    use std::collections::BTreeMap;

    fn append(builder: &mut tar::Builder<&mut Vec<u8>>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
    }

    fn extension_archive(architecture: &str, reference: &str) -> Vec<u8> {
        use hl_images::Digest;
        let document = format!(
            "name = \"daemon-{architecture}\"\ndisplay_name = \"Daemon {architecture}\"\nversion = \"1.2.3\"\nprotocol = 1\ncapabilities = [\"container-read\"]\n"
        );
        let mut layer = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut layer);
            append(&mut tar, "etc/husklet/extension.toml", document.as_bytes());
            tar.finish().unwrap();
        }
        let config = serde_json::to_vec(&serde_json::json!({
            "architecture": architecture, "os": "linux",
            "config": {
                "Entrypoint": [format!("/opt/husklet/{architecture}")],
                "Cmd": ["--serve"],
                "User": "65532:65532",
                "Labels": {
                    "husklet.extension.manifest": "/etc/husklet/extension.toml",
                    "husklet.extension.protocol": "1"
                }
            },
            "rootfs": {"type": "layers", "diff_ids": [Digest::sha256(&layer).to_string()]}
        }))
        .unwrap();
        let manifest = serde_json::to_vec(&serde_json::json!([{
            "Config": "config.json", "RepoTags": [reference], "Layers": ["layer.tar"]
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

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, contents.as_bytes())
                .expect("appended");
        }
        builder.into_inner().expect("archive")
    }

    #[test]
    fn an_unlabelled_image_is_read_at_the_default_path() {
        assert_eq!(manifest_path(&BTreeMap::new()), Manifest::DEFAULT_PATH);
    }

    #[test]
    fn a_labelled_image_is_read_where_it_says() {
        let labels = BTreeMap::from([(Manifest::LABEL.to_owned(), " /opt/extension.toml ".to_owned())]);
        assert_eq!(manifest_path(&labels), "/opt/extension.toml");
    }

    #[test]
    fn a_blank_label_falls_back_rather_than_reading_the_root() {
        let labels = BTreeMap::from([(Manifest::LABEL.to_owned(), String::new())]);
        assert_eq!(manifest_path(&labels), Manifest::DEFAULT_PATH);
    }

    #[test]
    fn the_archives_one_file_is_the_document() {
        let archive = archive(&[("extension.toml", "name = \"sample\"\n")]);
        assert_eq!(document(&archive).expect("document"), "name = \"sample\"\n");
    }

    #[test]
    fn an_archive_with_no_file_is_named_rather_than_read_as_empty() {
        let refusal = document(&archive(&[])).expect_err("nothing to read");
        assert!(refusal.contains("no manifest"), "got {refusal}");
    }

    #[test]
    fn registry_ports_are_not_mistaken_for_tags() {
        assert_eq!(split("localhost:5000/team/tool"), ("localhost:5000/team/tool", None));
        assert_eq!(
            split("localhost:5000/team/tool:edge"),
            ("localhost:5000/team/tool", Some("edge"))
        );
    }

    #[test]
    fn digests_are_forwarded_as_the_pull_selector() {
        assert_eq!(split("team/tool@sha256:abcd"), ("team/tool", Some("sha256:abcd")));
    }

    #[test]
    fn manifest_extraction_is_pinned_to_the_inspected_digest() {
        assert_eq!(
            immutable_content("registry/team:latest", "sha256:resolved").expect("digest"),
            "sha256:resolved"
        );
        assert!(
            immutable_content("registry/team:latest", "  ").is_err(),
            "a mutable tag cannot substitute for a missing digest"
        );
    }

    #[cfg(unix)]
    #[test]
    fn acquisition_pull_names_the_workspace_platform_on_the_docker_wire() {
        use std::io::{Read as _, Write as _};

        let root = tempfile::TempDir::new().unwrap();
        let socket = root.path().join("mock.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [r#"{"message":"No such image"}"#, "{\"error\":\"fixture stop\"}\n"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0, "request ended before its headers");
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let request = String::from_utf8(bytes).unwrap();
                requests.push(request.lines().next().unwrap().to_owned());
                let status = if requests.len() == 1 { "404 Not Found" } else { "200 OK" };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        let (progress, received) = std::sync::mpsc::channel();
        Candidate::acquire_from_socket(
            &socket,
            hl_ws::Arch::Arm64,
            "registry.test/team/extension:v1",
            &progress,
        );
        let terminal = received
            .into_iter()
            .find(|event| {
                matches!(
                    event,
                    Acquisition::Failed(_) | Acquisition::Ready(_) | Acquisition::Cancelled
                )
            })
            .unwrap();
        assert!(matches!(terminal, Acquisition::Failed(reason) if reason.contains("fixture stop")));
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0],
            "GET /v1.43/images/registry%2Etest%2Fteam%2Fextension%3Av1/json HTTP/1.1"
        );
        assert_eq!(
            requests[1],
            "POST /v1.43/images/create?fromImage=registry%2Etest%2Fteam%2Fextension&tag=v1&platform=linux%2Farm64 HTTP/1.1"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn both_architecture_candidates_are_bound_to_their_workspace_without_starting() {
        use super::super::sidecar::{Image, SidecarSpec, SIGNATURE_LABEL, SOCKET_TARGET, SOCKET_VARIABLE};
        use hl_client::model::EventQuery;
        use hl_container::{Config, Containers, Persistence};
        use hl_daemon::Daemon;
        use hl_images::format::docker::{Archive, Limits};
        use tokio::sync::oneshot;

        let root = tempfile::TempDir::new().unwrap();
        let containers = Containers::builder(Config::new(root.path()).persistence(Persistence::Memory))
            .build()
            .await
            .unwrap();
        Archive::load(
            &extension_archive("arm64", "scenario/extension:arm64")[..],
            &containers.images().unwrap(),
            Limits::default(),
        )
        .unwrap();
        Archive::load(
            &extension_archive("amd64", "scenario/extension:amd64")[..],
            &containers.images().unwrap(),
            Limits::default(),
        )
        .unwrap();
        let socket = root.path().join("candidate.sock");
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(Daemon::new(containers).server(&socket).serve_with_shutdown(async move {
            let _ = stopped.await;
        }));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(socket.exists(), "embedded daemon socket did not appear");

        let client = hl_client::Client::unix(&socket).unwrap();
        let acquire = |architecture, reference: &'static str| {
            let (progress, received) = std::sync::mpsc::channel();
            let acquisition_socket = socket.clone();
            let worker = std::thread::spawn(move || {
                Candidate::acquire_from_socket(&acquisition_socket, architecture, reference, &progress)
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let terminal = loop {
                let event = received
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("candidate acquisition timed out");
                if matches!(
                    event,
                    Acquisition::Ready(_) | Acquisition::Failed(_) | Acquisition::Cancelled
                ) {
                    break event;
                }
            };
            worker.join().unwrap();
            terminal
        };
        let Acquisition::Ready(candidate) = acquire(hl_ws::Arch::Arm64, "scenario/extension:arm64") else {
            panic!("arm64 candidate did not become ready")
        };
        assert_eq!(candidate.manifest.name.to_string(), "daemon-arm64");
        assert_eq!(candidate.manifest.version, "1.2.3");
        assert!(!candidate.digest.is_empty());

        let inspection = client.images().inspect(&candidate.digest).await.unwrap();
        assert_eq!(inspection.os, "linux");
        assert_eq!(inspection.architecture, "arm64");
        assert_eq!(inspection.config.entrypoint, ["/opt/husklet/arm64"]);
        assert_eq!(inspection.config.user, "65532:65532");
        let image = Image::from_inspection(candidate.digest.clone(), &inspection);
        let credential = root.path().join("credentials/daemon-candidate.sock");
        let spec = SidecarSpec::new(
            &candidate.manifest,
            &candidate.manifest.capabilities,
            &image,
            &credential,
        );
        let request = spec.request();
        let host = request.host_config.expect("sidecar host policy");
        assert_eq!(request.image, candidate.digest);
        assert_eq!(request.entrypoint, Some(vec!["/opt/husklet/arm64".to_owned()]));
        assert_eq!(request.user.as_deref(), Some("65532:65532"));
        assert_eq!(request.env, Some(vec![format!("{SOCKET_VARIABLE}={SOCKET_TARGET}")]));
        assert_eq!(host.network_mode, "none");
        assert_eq!(host.mounts.len(), 1);
        assert_eq!(host.mounts[0].source, credential.to_string_lossy());
        assert_eq!(host.mounts[0].target, SOCKET_TARGET);
        assert_eq!(request.labels.get(SIGNATURE_LABEL), Some(&spec.signature()));
        let ungranted = SidecarSpec::new(
            &candidate.manifest,
            &hl_extension::Grant::default(),
            &image,
            &credential,
        );
        assert_ne!(
            spec.signature(),
            ungranted.signature(),
            "consent is part of sidecar identity"
        );

        let amd_root = tempfile::TempDir::new().unwrap();
        let amd_containers = Containers::builder(Config::new(amd_root.path()).persistence(Persistence::Memory))
            .build()
            .await
            .unwrap();
        Archive::load(
            &extension_archive("arm64", "scenario/extension:arm64")[..],
            &amd_containers.images().unwrap(),
            Limits::default(),
        )
        .unwrap();
        Archive::load(
            &extension_archive("amd64", "scenario/extension:amd64")[..],
            &amd_containers.images().unwrap(),
            Limits::default(),
        )
        .unwrap();
        let amd_socket = amd_root.path().join("candidate.sock");
        let (amd_stop, amd_stopped) = oneshot::channel();
        let amd_server = tokio::spawn(
            Daemon::new(amd_containers)
                .platform(hl_images::Platform::linux_amd64())
                .server(&amd_socket)
                .serve_with_shutdown(async move {
                    let _ = amd_stopped.await;
                }),
        );
        for _ in 0..100 {
            if amd_socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let acquire_amd = |architecture, reference: &'static str| {
            let (progress, received) = std::sync::mpsc::channel();
            let acquisition_socket = amd_socket.clone();
            let worker = std::thread::spawn(move || {
                Candidate::acquire_from_socket(&acquisition_socket, architecture, reference, &progress)
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let terminal = loop {
                let event = received
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("amd64 candidate acquisition timed out");
                if matches!(
                    event,
                    Acquisition::Ready(_) | Acquisition::Failed(_) | Acquisition::Cancelled
                ) {
                    break event;
                }
            };
            worker.join().unwrap();
            terminal
        };
        let amd_client = hl_client::Client::unix(&amd_socket).unwrap();
        let amd64 = match acquire_amd(hl_ws::Arch::Amd64, "scenario/extension:amd64") {
            Acquisition::Ready(candidate) => candidate,
            other => panic!("amd64 candidate did not become ready: {other:?}"),
        };
        assert_eq!(amd64.manifest.name.to_string(), "daemon-amd64");
        assert_ne!(
            amd64.digest, candidate.digest,
            "architecture-specific artifacts have distinct identities"
        );
        let amd64_inspection = amd_client.images().inspect(&amd64.digest).await.unwrap();
        assert_eq!(amd64_inspection.os, "linux");
        assert_eq!(amd64_inspection.architecture, "amd64");
        assert_eq!(amd64_inspection.config.entrypoint, ["/opt/husklet/amd64"]);
        let amd64_image = Image::from_inspection(amd64.digest.clone(), &amd64_inspection);
        let amd64_spec = SidecarSpec::new(
            &amd64.manifest,
            &amd64.manifest.capabilities,
            &amd64_image,
            root.path().join("credentials/daemon-amd64.sock"),
        );
        assert_eq!(amd64_spec.request().image, amd64.digest);
        assert_ne!(amd64_spec.signature(), spec.signature());

        let Acquisition::Failed(mismatch) = acquire(hl_ws::Arch::Amd64, "scenario/extension:arm64") else {
            panic!("cross-architecture candidate was not refused")
        };
        assert!(mismatch.contains("linux/arm64"), "actual platform is named: {mismatch}");
        assert!(
            mismatch.contains("linux/amd64"),
            "required platform is named: {mismatch}"
        );
        assert!(
            client.containers().list(true).await.unwrap().is_empty(),
            "inspection container leaked"
        );

        let mut events = client
            .events()
            .subscribe(&EventQuery::default().since(0))
            .await
            .unwrap();
        let mut actions = Vec::new();
        for _ in 0..2 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            actions.push(event.action);
        }
        assert_eq!(actions, ["create", "destroy"]);
        assert!(
            actions.iter().all(|action| action != "start"),
            "unconsented image execution was attempted"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), events.next())
                .await
                .is_err(),
            "architecture rejection must happen before an inspection container is created"
        );

        drop(events);
        assert!(amd_client.containers().list(true).await.unwrap().is_empty());
        let mut amd_events = amd_client
            .events()
            .subscribe(&EventQuery::default().since(0))
            .await
            .unwrap();
        let mut amd_actions = Vec::new();
        for _ in 0..2 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), amd_events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            amd_actions.push(event.action);
        }
        assert_eq!(amd_actions, ["create", "destroy"]);
        assert!(amd_actions.iter().all(|action| action != "start"));
        drop(amd_events);
        amd_stop.send(()).unwrap();
        amd_server.await.unwrap().unwrap();
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
