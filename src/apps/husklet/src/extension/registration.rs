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
        let client = bridge.client();
        let _ = progress.send(Acquisition::Inspecting);
        cancellation.check()?;
        let inspection: InspectImage = match cancellable(&bridge, cancellation, client.images().inspect(reference))? {
            Ok(inspection) => inspection,
            Err(error) if matches!(super::failure(&error), HostError::Absent(_)) => {
                pull(&bridge, reference, progress, cancellation)?;
                cancellable(&bridge, cancellation, client.images().inspect(reference))?
                    .map_err(|error| error.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        cancellation.check()?;
        let _ = progress.send(Acquisition::ReadingManifest);
        let path = manifest_path(&inspection.config.labels);
        let archive = extract(&bridge, reference, &path, cancellation)?;
        cancellation.check()?;
        let manifest = Manifest::parse(&document(&archive)?, PROTOCOL).map_err(|invalid| invalid.to_string())?;
        Ok(Self {
            reference: reference.to_owned(),
            digest: inspection.id,
            manifest,
        })
    }
}

/// Pulls one registry reference, forwarding only progress the daemon actually
/// supplied. The daemon embeds registry failures in an otherwise successful
/// HTTP stream, so every record must be inspected.
fn pull(
    bridge: &Bridge,
    reference: &str,
    progress: &Sender<Acquisition>,
    cancellation: &Cancellation,
) -> Result<(), String> {
    let (name, tag) = split(reference);
    let client = bridge.client();
    let mut stream =
        cancellable(bridge, cancellation, client.images().pull(name, tag, None))?.map_err(|error| error.to_string())?;
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
    let _ = bridge.wait(async {
        tokio::time::timeout(CLEANUP_BOUND, client.containers().remove(&created.id, true, true)).await
    });
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
    use super::{document, manifest_path, split};
    use hl_extension::Manifest;
    use std::collections::BTreeMap;

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
}
