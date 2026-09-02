//! Listing and fetching images on behalf of an extension.

use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use hl_extension::port::{
    HostError, ImageDetails, ImagePruneResult, ImagePullChange, ImagePullJob, ImagePullStatus, ImageStore, ImageSummary,
};

use super::{failure, Bridge};

/// The image port over the workspace's container daemon.
pub struct ImageLibrary {
    bridge: Arc<Bridge>,
    pulls: Arc<Mutex<Pulls>>,
}

struct PullEntry {
    status: ImagePullStatus,
    cancel: Arc<PullCancellation>,
}
struct PullCancellation { cancelled: AtomicBool, wake: tokio::sync::Notify }
struct Pulls {
    next: u64,
    jobs: BTreeMap<u64, PullEntry>,
    changed: BTreeMap<u64, u64>,
}

impl ImageLibrary {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self {
            bridge,
            pulls: Arc::new(Mutex::new(Pulls {
                next: 1,
                jobs: BTreeMap::new(),
                changed: BTreeMap::new(),
            })),
        }
    }
}

impl ImageStore for ImageLibrary {
    /// # Errors
    /// Returns a host failure from the container daemon.
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        let client = self.bridge.client();
        let images = self
            .bridge
            .wait(client.images().list())
            .map_err(|error| failure(&error))?;
        Ok(images.iter().map(summary).collect())
    }

    /// Pulls a reference and reports the image it produced.
    ///
    /// The result is read back from the local listing rather than from the
    /// progress stream, so a pull and a list describe an image identically.
    ///
    /// # Errors
    /// Returns a host failure, including a registry refusal reported inside the
    /// progress stream, and `HostError::Absent` when the pull reported success
    /// but named no local image.
    fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
        let (name, tag) = split(reference);
        let client = self.bridge.client();
        self.bridge.wait(fetch(client, name, tag))?;
        let wanted = tagged(reference);
        self.list()?
            .into_iter()
            .find(|image| image.reference == wanted)
            .ok_or_else(|| HostError::Absent(format!("{reference} is not present after its pull")))
    }

    fn pull_start(&self, reference: &str) -> Result<ImagePullJob, HostError> {
        let reference = reference.trim();
        if reference.is_empty() || reference.len() > 512 {
            return Err(HostError::Conflict(
                "image reference must contain 1 to 512 bytes".into(),
            ));
        }
        let (job, cancel) = {
            let mut pulls = self.pulls.lock().unwrap();
            if pulls
                .jobs
                .values()
                .filter(|entry| !matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled"))
                .count()
                >= 4
            {
                return Err(HostError::Conflict("four image pulls are already active".into()));
            }
            while pulls.jobs.len() >= 32 {
                let Some(id) = pulls.jobs.iter().find_map(|(id, entry)| {
                    matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled").then_some(*id)
                }) else {
                    return Err(HostError::Conflict("image pull history is full".into()));
                };
                pulls.jobs.remove(&id);
                pulls.changed.remove(&id);
            }
            let id = pulls.next;
            pulls.next = pulls.next.saturating_add(1);
            let cancel = Arc::new(PullCancellation { cancelled: AtomicBool::new(false), wake: tokio::sync::Notify::new() });
            pulls.jobs.insert(
                id,
                PullEntry {
                    status: ImagePullStatus {
                        job: id.to_string(),
                        reference: reference.into(),
                        revision: 1,
                        state: "starting".into(),
                        status: None,
                        layer: None,
                        current: None,
                        total: None,
                        image: None,
                        error: None,
                    },
                    cancel: Arc::clone(&cancel),
                },
            );
            pulls.changed.insert(id, 1);
            (id, cancel)
        };
        let bridge = Arc::clone(&self.bridge);
        let registry = Arc::clone(&self.pulls);
        let reference = reference.to_owned();
        std::thread::spawn(move || {
            let outcome = bridge.wait(pull_job(&bridge, &reference, job, &registry, &cancel));
            finish_pull(&registry, job, &cancel, outcome);
        });
        Ok(ImagePullJob { job: job.to_string() })
    }

    fn pull_status(&self, job: &str) -> Result<ImagePullStatus, HostError> {
        let id = parse_job(job)?;
        self.pulls
            .lock()
            .unwrap()
            .jobs
            .get(&id)
            .map(|entry| entry.status.clone())
            .ok_or_else(|| HostError::Absent(format!("image pull job {job}")))
    }

    fn pull_cancel(&self, job: &str) -> Result<(), HostError> {
        let id = parse_job(job)?;
        let mut pulls = self.pulls.lock().unwrap();
        let entry = pulls
            .jobs
            .get_mut(&id)
            .ok_or_else(|| HostError::Absent(format!("image pull job {job}")))?;
        if matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled") {
            return Err(HostError::Conflict("image pull is already finished".into()));
        }
        entry.cancel.cancelled.store(true, Ordering::Release);
        entry.cancel.wake.notify_waiters();
        entry.status.revision = entry.status.revision.saturating_add(1);
        entry.status.state = "cancelled".into();
        *pulls.changed.entry(id).or_default() += 1;
        Ok(())
    }

    fn pull_changes(&self) -> Vec<ImagePullChange> {
        let mut pulls = self.pulls.lock().unwrap();
        let changed = std::mem::take(&mut pulls.changed);
        changed
            .into_iter()
            .filter_map(|(id, count)| {
                pulls.jobs.get(&id).map(|entry| ImagePullChange {
                    job: id.to_string(),
                    revision: entry.status.revision,
                    state: entry.status.state.clone(),
                    coalesced: count.saturating_sub(1),
                })
            })
            .collect()
    }

    fn inspect(&self, reference: &str) -> Result<ImageDetails, HostError> {
        let client = self.bridge.client();
        let image = self
            .bridge
            .wait(client.images().inspect(reference))
            .map_err(|error| failure(&error))?;
        Ok(ImageDetails {
            id: image.id,
            references: image
                .repo_tags
                .into_iter()
                .chain(image.repo_digests)
                .take(128)
                .collect(),
            created: image.created,
            size: u64::try_from(image.size).unwrap_or_default(),
            os: image.os,
            architecture: image.architecture,
            entrypoint: image.config.entrypoint.into_iter().take(128).collect(),
            command: image.config.cmd.into_iter().take(128).collect(),
            working_directory: image.config.working_dir,
            user: image.config.user,
        })
    }

    fn remove(&self, reference: &str) -> Result<(), HostError> {
        let client = self.bridge.client();
        self.bridge
            .wait(client.images().remove(reference))
            .map(|_| ())
            .map_err(|error| failure(&error))
    }

    fn prune(&self) -> Result<ImagePruneResult, HostError> {
        let client = self.bridge.client();
        let result = self
            .bridge
            .wait(client.images().prune())
            .map_err(|error| failure(&error))?;
        Ok(ImagePruneResult {
            deleted: u64::try_from(result.images_deleted.len()).unwrap_or(u64::MAX),
            space_reclaimed: u64::try_from(result.space_reclaimed).unwrap_or_default(),
        })
    }
}

fn parse_job(job: &str) -> Result<u64, HostError> {
    job.parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or_else(|| HostError::Conflict("invalid image pull job".into()))
}

fn update_pull(registry: &Arc<Mutex<Pulls>>, job: u64, change: impl FnOnce(&mut ImagePullStatus)) {
    let mut pulls = registry.lock().unwrap();
    if let Some(entry) = pulls.jobs.get_mut(&job) {
        entry.status.revision = entry.status.revision.saturating_add(1);
        change(&mut entry.status);
        *pulls.changed.entry(job).or_default() += 1;
    }
}

fn finish_pull(registry: &Arc<Mutex<Pulls>>, job: u64, cancel: &PullCancellation, outcome: Result<ImageSummary, HostError>) {
    if cancel.cancelled.load(Ordering::Acquire) {
        update_pull(registry, job, |status| status.state = "cancelled".into());
        return;
    }
    match outcome {
        Ok(image) => update_pull(registry, job, |status| {
            status.state = "complete".into();
            status.status = Some("Pull complete".into());
            status.image = Some(image);
        }),
        Err(error) => update_pull(registry, job, |status| {
            status.state = "failed".into();
            status.error = Some(error.to_string());
        }),
    }
}

async fn pull_job(
    bridge: &Bridge,
    reference: &str,
    job: u64,
    registry: &Arc<Mutex<Pulls>>,
    cancel: &PullCancellation,
) -> Result<ImageSummary, HostError> {
    let (name, tag) = split(reference);
    let client = bridge.client();
    let images = client.images();
    let mut stream = tokio::select! {
        value = images.pull(name, tag, None) => value.map_err(|error| failure(&error))?,
        () = cancel.wake.notified() => return Err(HostError::Conflict("image pull cancelled".into())),
    };
    loop {
        if cancel.cancelled.load(Ordering::Acquire) {
            return Err(HostError::Conflict("image pull cancelled".into()));
        }
        let record = tokio::select! {
            value = stream.next() => value.map_err(|error| failure(&error))?,
            () = cancel.wake.notified() => return Err(HostError::Conflict("image pull cancelled".into())),
        };
        let Some(record) = record else { break };
        if let Some(error) = record.error {
            return Err(HostError::Failed(error));
        }
        update_pull(registry, job, |value| {
            value.state = "pulling".into();
            value.status = record.status;
            value.layer = record.id;
            value.current = record
                .progress_detail
                .as_ref()
                .and_then(|p| u64::try_from(p.current).ok());
            value.total = record.progress_detail.and_then(|p| u64::try_from(p.total).ok());
        });
    }
    let wanted = tagged(reference);
    client
        .images()
        .list()
        .await
        .map_err(|error| failure(&error))?
        .iter()
        .map(summary)
        .find(|image| image.reference == wanted)
        .ok_or_else(|| HostError::Absent(format!("{reference} is not present after its pull")))
}

/// Runs a pull to completion, surfacing the registry's own refusal.
///
/// Docker reports registry failures as records inside a successful stream, so a
/// pull that is never inspected looks like a pull that worked.
async fn fetch(client: &hl_client::Client, name: &str, tag: Option<&str>) -> Result<(), HostError> {
    let mut progress = client
        .images()
        .pull(name, tag, None)
        .await
        .map_err(|error| failure(&error))?;
    while let Some(record) = progress.next().await.map_err(|error| failure(&error))? {
        let Some(detail) = record.error else { continue };
        return Err(HostError::Failed(detail));
    }
    Ok(())
}

/// Maps a Docker image entry onto the protocol's image view.
fn summary(image: &hl_client::model::ImageSummary) -> ImageSummary {
    ImageSummary {
        id: image.id.clone(),
        reference: image.name(),
        size: u64::try_from(image.size).unwrap_or_default(),
        created: image.created,
    }
}

/// Splits a reference into the name and the tag or digest the registry wants.
///
/// The colon in a registry host's port belongs to the name, so only the part
/// after the final path separator is considered.
fn split(reference: &str) -> (&str, Option<&str>) {
    if let Some(index) = reference.find('@') {
        return (&reference[..index], Some(&reference[index + 1..]));
    }
    let start = reference.rfind('/').map_or(0, |index| index + 1);
    let Some(index) = reference[start..].rfind(':') else {
        return (reference, None);
    };
    let index = start + index;
    (&reference[..index], Some(&reference[index + 1..]))
}

/// The reference as the local listing spells it, with Docker's implied tag made
/// explicit so an untagged request still matches what was pulled.
fn tagged(reference: &str) -> String {
    match split(reference) {
        (_, Some(_)) => reference.to_owned(),
        (name, None) => format!("{name}:latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<Mutex<Pulls>> {
        Arc::new(Mutex::new(Pulls {
            next: 2,
            jobs: BTreeMap::from([(
                1,
                PullEntry {
                    status: ImagePullStatus {
                        job: "1".into(),
                        reference: "alpine".into(),
                        revision: 1,
                        state: "starting".into(),
                        status: None,
                        layer: None,
                        current: None,
                        total: None,
                        image: None,
                        error: None,
                    },
                    cancel: Arc::new(PullCancellation { cancelled: AtomicBool::new(false), wake: tokio::sync::Notify::new() }),
                },
            )]),
            changed: BTreeMap::new(),
        }))
    }

    #[test]
    fn cancelled_pull_can_never_publish_success_afterward() {
        let registry = registry();
        let cancelled = PullCancellation { cancelled: AtomicBool::new(true), wake: tokio::sync::Notify::new() };
        finish_pull(
            &registry,
            1,
            &cancelled,
            Ok(ImageSummary {
                id: "i1".into(),
                reference: "alpine:latest".into(),
                size: 1,
                created: 0,
            }),
        );
        let pulls = registry.lock().unwrap();
        let status = &pulls.jobs[&1].status;
        assert_eq!(status.state, "cancelled");
        assert!(status.image.is_none());
    }

    #[test]
    fn frequent_progress_coalesces_to_one_latest_invalidation() {
        let registry = registry();
        update_pull(&registry, 1, |status| status.current = Some(1));
        update_pull(&registry, 1, |status| status.current = Some(2));
        let mut pulls = registry.lock().unwrap();
        let pending = std::mem::take(&mut pulls.changed);
        assert_eq!(pulls.jobs[&1].status.current, Some(2));
        assert_eq!(pending[&1], 2);
    }

    #[test]
    fn a_reference_splits_into_a_name_and_a_tag() {
        assert_eq!(split("ubuntu"), ("ubuntu", None));
        assert_eq!(split("ubuntu:24.04"), ("ubuntu", Some("24.04")));
        assert_eq!(split("library/ubuntu:24.04"), ("library/ubuntu", Some("24.04")));
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(
            split("registry.example:5000/ubuntu"),
            ("registry.example:5000/ubuntu", None)
        );
        assert_eq!(
            split("registry.example:5000/ubuntu:24.04"),
            ("registry.example:5000/ubuntu", Some("24.04"))
        );
    }

    #[test]
    fn a_digest_reference_keeps_its_whole_digest() {
        assert_eq!(split("ubuntu@sha256:abc"), ("ubuntu", Some("sha256:abc")));
    }

    #[test]
    fn an_untagged_request_matches_the_tag_docker_implies() {
        assert_eq!(tagged("ubuntu"), "ubuntu:latest");
        assert_eq!(tagged("ubuntu:24.04"), "ubuntu:24.04");
    }

    #[test]
    fn an_image_entry_maps_onto_the_protocol_view() {
        let image: hl_client::model::ImageSummary = serde_json::from_value(serde_json::json!({
            "Id": "sha256:deadbeefcafe0000",
            "RepoTags": ["ubuntu:24.04"],
            "RepoDigests": [],
            "Created": 1_700_000_000_i64,
            "Size": 80_000_000_i64,
            "SharedSize": 0_i64,
            "VirtualSize": 80_000_000_i64,
            "Labels": {},
            "Containers": 0_i64
        }))
        .expect("image listing");

        let mapped = summary(&image);
        assert_eq!(mapped.id, "sha256:deadbeefcafe0000");
        assert_eq!(mapped.reference, "ubuntu:24.04");
        assert_eq!(mapped.size, 80_000_000);
        assert_eq!(mapped.created, 1_700_000_000);
    }
}
