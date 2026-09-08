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
    owner: String,
    status: ImagePullStatus,
    cancel: Arc<PullCancellation>,
}
struct PullCancellation {
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}
struct Pulls {
    next_change: u64,
    jobs: BTreeMap<String, PullEntry>,
    changed: BTreeMap<String, (u64, u64)>,
}

impl ImageLibrary {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self {
            bridge,
            pulls: Arc::new(Mutex::new(Pulls {
                jobs: BTreeMap::new(),
                changed: BTreeMap::new(),
                next_change: 1,
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

    fn pull_start(&self, owner: &str, reference: &str) -> Result<ImagePullJob, HostError> {
        if owner.is_empty() {
            return Err(HostError::Conflict("image pull owner is missing".into()));
        }
        let reference = reference
            .parse::<hl_image_reference::ImageReference>()
            .map_err(|error| HostError::Conflict(format!("invalid image reference: {error}")))?
            .to_string();
        let (job, cancel) = {
            let mut pulls = self.pulls.lock().unwrap();
            admit_pull(&mut pulls, owner)?;
            let id = uuid::Uuid::new_v4().simple().to_string();
            let cancel = Arc::new(PullCancellation {
                cancelled: AtomicBool::new(false),
                wake: tokio::sync::Notify::new(),
            });
            pulls.jobs.insert(
                id.clone(),
                PullEntry {
                    owner: owner.to_owned(),
                    status: ImagePullStatus {
                        job: id.clone(),
                        reference: reference.clone(),
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
            note_change(&mut pulls, &id);
            (id, cancel)
        };
        let bridge = Arc::clone(&self.bridge);
        let registry = Arc::clone(&self.pulls);
        let worker_job = job.clone();
        std::thread::spawn(move || {
            let outcome = bridge.wait(pull_job(&bridge, &reference, &worker_job, &registry, &cancel));
            finish_pull(&registry, &worker_job, &cancel, outcome);
        });
        Ok(ImagePullJob { job })
    }

    fn pull_status(&self, owner: &str, job: &str) -> Result<ImagePullStatus, HostError> {
        let id = parse_job(job)?;
        self.pulls
            .lock()
            .unwrap()
            .jobs
            .get(id)
            .filter(|entry| entry.owner == owner)
            .map(|entry| entry.status.clone())
            .ok_or_else(|| HostError::Absent("image pull job is absent".into()))
    }

    fn pull_cancel(&self, owner: &str, job: &str) -> Result<(), HostError> {
        let id = parse_job(job)?;
        let mut pulls = self.pulls.lock().unwrap();
        let entry = pulls
            .jobs
            .get_mut(id)
            .filter(|entry| entry.owner == owner)
            .ok_or_else(|| HostError::Absent("image pull job is absent".into()))?;
        if matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled") {
            return Err(HostError::Conflict("image pull is already finished".into()));
        }
        entry.cancel.cancelled.store(true, Ordering::Release);
        entry.cancel.wake.notify_one();
        entry.status.revision = entry.status.revision.saturating_add(1);
        entry.status.state = "cancelled".into();
        note_change(&mut pulls, id);
        Ok(())
    }

    fn pull_changes(&self, owner: &str, after: u64) -> Vec<ImagePullChange> {
        let pulls = self.pulls.lock().unwrap();
        pull_changes_since(&pulls, owner, after)
    }

    fn inspect(&self, reference: &str) -> Result<ImageDetails, HostError> {
        let client = self.bridge.client();
        let image = self
            .bridge
            .wait(client.images().inspect(reference))
            .map_err(|error| failure(&error))?;
        Ok(ImageDetails {
            id: image.id,
            references: canonical_aliases(image.repo_tags.iter().chain(&image.repo_digests)),
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

fn parse_job(job: &str) -> Result<&str, HostError> {
    (job.len() == 32
        && job
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(job)
    .ok_or_else(|| HostError::Absent("image pull job is absent".into()))
}

fn active_pulls(pulls: &Pulls, owner: &str) -> usize {
    pulls
        .jobs
        .values()
        .filter(|entry| {
            entry.owner == owner && !matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled")
        })
        .count()
}

fn note_change(pulls: &mut Pulls, job: &str) {
    let sequence = pulls.next_change;
    pulls.next_change = pulls.next_change.saturating_add(1);
    let count = pulls.changed.get(job).map_or(1, |(_, count)| count.saturating_add(1));
    pulls.changed.insert(job.to_owned(), (sequence, count));
}

fn pull_changes_since(pulls: &Pulls, owner: &str, after: u64) -> Vec<ImagePullChange> {
    let mut changes = pulls
        .changed
        .iter()
        .filter(|(_, (sequence, _))| *sequence > after)
        .filter_map(|(id, (sequence, count))| {
            pulls
                .jobs
                .get(id)
                .filter(|entry| entry.owner == owner)
                .map(|entry| ImagePullChange {
                    sequence: *sequence,
                    job: id.clone(),
                    revision: entry.status.revision,
                    state: entry.status.state.clone(),
                    coalesced: count.saturating_sub(1),
                })
        })
        .collect::<Vec<_>>();
    changes.sort_by_key(|change| change.sequence);
    changes.truncate(64);
    changes
}

fn update_pull(registry: &Arc<Mutex<Pulls>>, job: &str, change: impl FnOnce(&mut ImagePullStatus)) {
    let mut pulls = registry.lock().unwrap();
    if let Some(entry) = pulls.jobs.get_mut(job) {
        entry.status.revision = entry.status.revision.saturating_add(1);
        change(&mut entry.status);
        note_change(&mut pulls, job);
    }
}

fn finish_pull(
    registry: &Arc<Mutex<Pulls>>,
    job: &str,
    cancel: &PullCancellation,
    outcome: Result<ImageSummary, HostError>,
) {
    if cancel.cancelled.load(Ordering::Acquire) {
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

fn oldest_terminal_job(pulls: &Pulls, preferred_owner: Option<&str>) -> Option<String> {
    pulls
        .jobs
        .iter()
        .filter(|(_, entry)| preferred_owner.is_none_or(|owner| entry.owner == owner))
        .filter(|(_, entry)| matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled"))
        .min_by_key(|(id, _)| pulls.changed.get(*id).map_or(u64::MAX, |change| change.0))
        .map(|(id, _)| id.clone())
}

fn admit_pull(pulls: &mut Pulls, owner: &str) -> Result<(), HostError> {
    if active_pulls(pulls, owner) >= 4 {
        return Err(HostError::Conflict("four image pulls are already active".into()));
    }
    let globally_active = pulls
        .jobs
        .values()
        .filter(|entry| !matches!(entry.status.state.as_str(), "complete" | "failed" | "cancelled"))
        .count();
    if globally_active >= 16 {
        return Err(HostError::Conflict(
            "sixteen image pulls are already active globally".into(),
        ));
    }
    while pulls.jobs.values().filter(|entry| entry.owner == owner).count() >= 32 {
        let Some(id) = oldest_terminal_job(pulls, Some(owner)) else {
            return Err(HostError::Conflict(
                "this extension's image pull history is full".into(),
            ));
        };
        pulls.jobs.remove(&id);
        pulls.changed.remove(&id);
    }
    while pulls.jobs.len() >= 128 {
        let Some(id) = oldest_terminal_job(pulls, Some(owner)).or_else(|| oldest_terminal_job(pulls, None)) else {
            return Err(HostError::Conflict("global image pull history is full".into()));
        };
        pulls.jobs.remove(&id);
        pulls.changed.remove(&id);
    }
    Ok(())
}

async fn pull_job(
    bridge: &Bridge,
    reference: &str,
    job: &str,
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

/// Maps a Docker image entry onto the protocol's image view.
fn summary(image: &hl_client::model::ImageSummary) -> ImageSummary {
    let references = canonical_aliases(image.repo_tags.iter().chain(&image.repo_digests));
    ImageSummary {
        id: image.id.clone(),
        reference: references.first().cloned().unwrap_or_else(|| image.id.clone()),
        references,
        size: u64::try_from(image.size).unwrap_or_default(),
        created: image.created,
    }
}

fn canonical_aliases<'a>(aliases: impl Iterator<Item = &'a String>) -> Vec<String> {
    aliases
        .filter_map(|reference| reference.parse::<hl_image_reference::ImageReference>().ok())
        .map(|reference| reference.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(128)
        .collect()
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
        let job = "11111111111111111111111111111111".to_owned();
        Arc::new(Mutex::new(Pulls {
            next_change: 1,
            jobs: BTreeMap::from([(
                job.clone(),
                PullEntry {
                    owner: "test".into(),
                    status: ImagePullStatus {
                        job,
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
                    cancel: Arc::new(PullCancellation {
                        cancelled: AtomicBool::new(false),
                        wake: tokio::sync::Notify::new(),
                    }),
                },
            )]),
            changed: BTreeMap::new(),
        }))
    }

    #[test]
    fn cancelled_pull_can_never_publish_success_afterward() {
        let registry = registry();
        registry
            .lock()
            .unwrap()
            .jobs
            .get_mut("11111111111111111111111111111111")
            .unwrap()
            .status
            .state = "cancelled".into();
        let cancelled = PullCancellation {
            cancelled: AtomicBool::new(true),
            wake: tokio::sync::Notify::new(),
        };
        finish_pull(
            &registry,
            "11111111111111111111111111111111",
            &cancelled,
            Ok(ImageSummary {
                id: "i1".into(),
                reference: "alpine:latest".into(),
                references: vec!["alpine:latest".into()],
                size: 1,
                created: 0,
            }),
        );
        let pulls = registry.lock().unwrap();
        let status = &pulls.jobs["11111111111111111111111111111111"].status;
        assert_eq!(status.state, "cancelled");
        assert!(status.image.is_none());
    }

    #[test]
    fn frequent_progress_coalesces_to_one_latest_invalidation() {
        let registry = registry();
        update_pull(&registry, "11111111111111111111111111111111", |status| {
            status.current = Some(1)
        });
        update_pull(&registry, "11111111111111111111111111111111", |status| {
            status.current = Some(2)
        });
        let mut pulls = registry.lock().unwrap();
        let pending = std::mem::take(&mut pulls.changed);
        assert_eq!(pulls.jobs["11111111111111111111111111111111"].status.current, Some(2));
        assert_eq!(pending["11111111111111111111111111111111"].1, 2);
    }

    #[test]
    fn pull_changes_are_owner_scoped_non_destructive_and_cursor_bound() {
        let registry = registry();
        {
            let mut pulls = registry.lock().unwrap();
            note_change(&mut pulls, "11111111111111111111111111111111");
            let first = pull_changes_since(&pulls, "test", 0);
            let second_subscriber = pull_changes_since(&pulls, "test", 0);
            assert_eq!(first, second_subscriber);
            assert_eq!(first.len(), 1);
            assert!(pull_changes_since(&pulls, "foreign", 0).is_empty());
            assert!(pull_changes_since(&pulls, "test", first[0].sequence).is_empty());
        }
    }

    #[test]
    fn malformed_and_foreign_job_identity_are_indistinguishable() {
        assert!(matches!(parse_job("not-a-job"), Err(HostError::Absent(_))));
        let pulls = registry();
        let pulls = pulls.lock().unwrap();
        let job = parse_job("22222222222222222222222222222222").unwrap();
        assert!(pulls.jobs.get(job).filter(|entry| entry.owner == "foreign").is_none());
    }

    #[test]
    fn one_owner_saturating_its_quota_does_not_block_another_owner() {
        let registry = registry();
        let mut pulls = registry.lock().unwrap();
        for suffix in 2..=4 {
            let job = format!("{suffix:032}");
            pulls.jobs.insert(
                job.clone(),
                PullEntry {
                    owner: "test".into(),
                    status: ImagePullStatus {
                        job,
                        reference: "docker.io/library/alpine:latest".into(),
                        revision: 1,
                        state: "starting".into(),
                        status: None,
                        layer: None,
                        current: None,
                        total: None,
                        image: None,
                        error: None,
                    },
                    cancel: Arc::new(PullCancellation {
                        cancelled: AtomicBool::new(false),
                        wake: tokio::sync::Notify::new(),
                    }),
                },
            );
        }
        assert_eq!(active_pulls(&pulls, "test"), 4);
        assert_eq!(active_pulls(&pulls, "other"), 0);
    }

    #[test]
    fn full_global_history_evicts_oldest_terminal_entry_but_never_an_active_job() {
        let registry = registry();
        let mut pulls = registry.lock().unwrap();
        pulls.jobs.clear();
        pulls.changed.clear();
        for number in 0_u64..128 {
            let job = format!("{number:032x}");
            let active = number == 127;
            pulls.jobs.insert(
                job.clone(),
                PullEntry {
                    owner: format!("owner-{}", number % 5),
                    status: ImagePullStatus {
                        job: job.clone(),
                        reference: "docker.io/library/alpine:latest".into(),
                        revision: 1,
                        state: if active { "pulling" } else { "complete" }.into(),
                        status: None,
                        layer: None,
                        current: None,
                        total: None,
                        image: None,
                        error: None,
                    },
                    cancel: Arc::new(PullCancellation {
                        cancelled: AtomicBool::new(false),
                        wake: tokio::sync::Notify::new(),
                    }),
                },
            );
            pulls.changed.insert(job, (number + 1, 1));
        }

        admit_pull(&mut pulls, "new-owner").unwrap();

        assert_eq!(pulls.jobs.len(), 127);
        assert!(!pulls.jobs.contains_key("00000000000000000000000000000000"));
        assert!(pulls.jobs.contains_key("0000000000000000000000000000007f"));
        assert_eq!(active_pulls(&pulls, "owner-2"), 1);
    }

    #[test]
    fn daemon_shorthand_aliases_are_canonicalized_before_authority_checks() {
        let image = hl_client::model::ImageSummary {
            id: format!("sha256:{}", "a".repeat(64)),
            repo_tags: vec!["alpine:3.20".into()],
            ..hl_client::model::ImageSummary::default()
        };
        let projected = summary(&image);
        assert_eq!(projected.reference, "docker.io/library/alpine:3.20");
        assert_eq!(projected.references, vec!["docker.io/library/alpine:3.20"]);
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
        assert_eq!(mapped.reference, "docker.io/library/ubuntu:24.04");
        assert_eq!(mapped.references, vec!["docker.io/library/ubuntu:24.04"]);
        assert_eq!(mapped.size, 80_000_000);
        assert_eq!(mapped.created, 1_700_000_000);
    }
}
