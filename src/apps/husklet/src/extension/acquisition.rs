//! Bounded, asynchronous image acquisition awaiting explicit user consent.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex, PoisonError};

use hl_extension::port::HostError;
use hl_extension::Grant;

use super::management_events::ExtensionEvents;
use super::{Acquisition, Cancellation, Candidate, Roster};
use crate::config::WorkspaceConfig;

type Acquire = dyn Fn(&WorkspaceConfig, &str, &mpsc::Sender<Acquisition>, &Cancellation) + Send + Sync;

/// Opaque identity of one bounded acquisition job.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AcquisitionJob(u64);

impl AcquisitionJob {
    pub(crate) fn parse(value: &str) -> Result<Self, HostError> {
        let id = value
            .parse::<u64>()
            .map_err(|_| HostError::Conflict("invalid extension acquisition job".into()))?;
        (id != 0)
            .then_some(Self(id))
            .ok_or_else(|| HostError::Conflict("invalid extension acquisition job".into()))
    }

    pub(crate) fn wire(self) -> String {
        self.0.to_string()
    }

    #[cfg(test)]
    pub(crate) const fn test(value: u64) -> Self {
        Self(value)
    }
}

/// Consent-relevant fields observed from one exact image digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcquisitionCandidate {
    pub reference: String,
    pub digest: String,
    pub name: String,
    pub version: String,
    pub requested: Grant,
}

/// A revisioned snapshot suitable for polling across a socket boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcquisitionSnapshot {
    pub reference: String,
    pub revision: u64,
    pub state: AcquisitionState,
}

/// The latest truthful state reported by an acquisition worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AcquisitionState {
    Inspecting,
    Pulling {
        status: String,
        id: Option<String>,
        current: Option<u64>,
        total: Option<u64>,
    },
    ReadingManifest,
    Ready(AcquisitionCandidate),
    Committing,
    Installed,
    Updated,
    Failed(String),
    Cancelled,
}

impl AcquisitionState {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Installed | Self::Updated | Self::Failed(_) | Self::Cancelled
        )
    }

    pub(crate) const fn wire_state(&self) -> &'static str {
        match self {
            Self::Inspecting => "inspecting",
            Self::Pulling { .. } => "pulling",
            Self::ReadingManifest => "reading-manifest",
            Self::Ready(_) => "ready",
            Self::Committing => "committing",
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

struct Job {
    snapshot: AcquisitionSnapshot,
    candidate: Option<Candidate>,
    cancellation: Cancellation,
}

struct Registry {
    next: u64,
    jobs: BTreeMap<AcquisitionJob, Job>,
}

/// Native acquisition service used by the socket adapter.
///
/// Starting work is non-blocking. At most four acquisitions run at once and at
/// most 32 jobs are retained. A ready candidate moves to `Committing` before
/// storage is touched, so two callers cannot apply one consent decision twice.
pub(crate) struct ExtensionAcquisitions {
    workspace: WorkspaceConfig,
    registry: Arc<Mutex<Registry>>,
    acquire: Arc<Acquire>,
    events: ExtensionEvents,
}

impl ExtensionAcquisitions {
    pub(crate) const ACTIVE_LIMIT: usize = 4;
    pub(crate) const RETAINED_LIMIT: usize = 32;
    pub(crate) const REFERENCE_LIMIT: usize = 512;

    // The socket trait lands separately; keep the production constructor ready
    // without pretending the current protocol already routes these jobs.
    #[allow(dead_code)]
    pub(crate) fn new(workspace: &WorkspaceConfig, events: ExtensionEvents) -> Self {
        Self::with_acquirer_and_events(workspace, events, |workspace, reference, progress, cancellation| {
            Candidate::acquire_cancellable(workspace, reference, progress, cancellation);
        })
    }

    fn with_acquirer(
        workspace: &WorkspaceConfig,
        acquire: impl Fn(&WorkspaceConfig, &str, &mpsc::Sender<Acquisition>, &Cancellation) + Send + Sync + 'static,
    ) -> Self {
        Self::with_acquirer_and_events(workspace, ExtensionEvents::default(), acquire)
    }

    fn with_acquirer_and_events(
        workspace: &WorkspaceConfig,
        events: ExtensionEvents,
        acquire: impl Fn(&WorkspaceConfig, &str, &mpsc::Sender<Acquisition>, &Cancellation) + Send + Sync + 'static,
    ) -> Self {
        Self {
            workspace: workspace.clone(),
            registry: Arc::new(Mutex::new(Registry {
                next: 1,
                jobs: BTreeMap::new(),
            })),
            acquire: Arc::new(acquire),
            events,
        }
    }

    /// Queues acquisition without waiting for the daemon or registry.
    pub(crate) fn start(&self, reference: &str) -> Result<AcquisitionJob, HostError> {
        let reference = reference.trim();
        if reference.is_empty() || reference.len() > Self::REFERENCE_LIMIT {
            return Err(HostError::Conflict(format!(
                "image reference must contain 1 to {} bytes",
                Self::REFERENCE_LIMIT
            )));
        }
        let cancellation = Cancellation::default();
        let job = {
            let mut registry = self.lock();
            let active = registry
                .jobs
                .values()
                .filter(|job| !job.snapshot.state.terminal())
                .count();
            if active >= Self::ACTIVE_LIMIT {
                return Err(HostError::Conflict(
                    "four extension acquisitions are already active".into(),
                ));
            }
            while registry.jobs.len() >= Self::RETAINED_LIMIT {
                let removable = registry
                    .jobs
                    .iter()
                    .find_map(|(id, job)| job.snapshot.state.terminal().then_some(*id));
                let Some(removable) = removable else {
                    return Err(HostError::Conflict("extension acquisition history is full".into()));
                };
                registry.jobs.remove(&removable);
            }
            let id = AcquisitionJob(registry.next);
            registry.next = registry.next.saturating_add(1);
            registry.jobs.insert(
                id,
                Job {
                    snapshot: AcquisitionSnapshot {
                        reference: reference.to_owned(),
                        revision: 1,
                        state: AcquisitionState::Inspecting,
                    },
                    candidate: None,
                    cancellation: cancellation.clone(),
                },
            );
            id
        };
        self.events
            .acquisition(job, self.status(job).expect("new jobs are retained"));

        let (send, receive) = mpsc::channel();
        let acquire = Arc::clone(&self.acquire);
        let workspace = self.workspace.clone();
        let reference = reference.to_owned();
        let worker_cancel = cancellation.clone();
        std::thread::spawn(move || acquire(&workspace, &reference, &send, &worker_cancel));
        let registry = Arc::clone(&self.registry);
        let events = self.events.clone();
        std::thread::spawn(move || {
            while let Ok(event) = receive.recv() {
                let mut registry = registry.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(current) = registry.jobs.get_mut(&job) else {
                    break;
                };
                if matches!(current.snapshot.state, AcquisitionState::Cancelled) {
                    break;
                }
                let (state, candidate) = snapshot(event);
                current.candidate = candidate;
                current.snapshot.revision = current.snapshot.revision.saturating_add(1);
                current.snapshot.state = state;
                let changed = current.snapshot.clone();
                events.acquisition(job, changed);
                if current.snapshot.state.terminal() || matches!(current.snapshot.state, AcquisitionState::Ready(_)) {
                    break;
                }
            }
        });
        Ok(job)
    }

    pub(crate) fn status(&self, job: AcquisitionJob) -> Result<AcquisitionSnapshot, HostError> {
        self.lock()
            .jobs
            .get(&job)
            .map(|job| job.snapshot.clone())
            .ok_or_else(|| HostError::Absent(format!("extension acquisition {}", job.0)))
    }

    pub(crate) fn cancel(&self, job: AcquisitionJob) -> Result<(), HostError> {
        let mut registry = self.lock();
        let current = registry
            .jobs
            .get_mut(&job)
            .ok_or_else(|| HostError::Absent(format!("extension acquisition {}", job.0)))?;
        if matches!(
            current.snapshot.state,
            AcquisitionState::Committing | AcquisitionState::Installed | AcquisitionState::Updated
        ) {
            return Err(HostError::Conflict("the acquisition is already being committed".into()));
        }
        current.cancellation.cancel();
        current.snapshot.revision = current.snapshot.revision.saturating_add(1);
        current.snapshot.state = AcquisitionState::Cancelled;
        let snapshot = current.snapshot.clone();
        self.events.acquisition(job, snapshot);
        Ok(())
    }

    pub(crate) fn install(&self, job: AcquisitionJob, revision: u64, consented: &Grant) -> Result<(), HostError> {
        let candidate = self.take_ready(job, revision)?;
        let result = Roster::workspace(&self.workspace)
            .and_then(|mut roster| roster.register(&candidate.manifest, &candidate.digest, consented, moment()))
            .map_err(|error| HostError::Failed(error.to_string()));
        self.finish(job, result, AcquisitionState::Installed)
    }

    pub(crate) fn update(&self, job: AcquisitionJob, revision: u64, consented: &Grant) -> Result<(), HostError> {
        let candidate = self.take_ready(job, revision)?;
        let result = (|| {
            let mut roster = Roster::workspace(&self.workspace).map_err(|error| error.to_string())?;
            let update = roster
                .prepare_update(&candidate.manifest, &candidate.digest)
                .map_err(|error| error.to_string())?;
            roster
                .commit_update(update, consented, moment())
                .map_err(|error| error.to_string())
        })()
        .map_err(HostError::Failed);
        self.finish(job, result, AcquisitionState::Updated)
    }

    fn take_ready(&self, job: AcquisitionJob, revision: u64) -> Result<Candidate, HostError> {
        let mut registry = self.lock();
        let current = registry
            .jobs
            .get_mut(&job)
            .ok_or_else(|| HostError::Absent(format!("extension acquisition {}", job.0)))?;
        if current.snapshot.revision != revision {
            return Err(HostError::Conflict("the acquisition revision has changed".into()));
        }
        if !matches!(current.snapshot.state, AcquisitionState::Ready(_)) {
            return Err(HostError::Conflict("the acquisition is not awaiting consent".into()));
        }
        let candidate = current
            .candidate
            .take()
            .expect("ready snapshots retain their exact candidate");
        current.snapshot.revision = current.snapshot.revision.saturating_add(1);
        current.snapshot.state = AcquisitionState::Committing;
        let snapshot = current.snapshot.clone();
        self.events.acquisition(job, snapshot);
        Ok(candidate)
    }

    fn finish(
        &self,
        job: AcquisitionJob,
        result: Result<(), HostError>,
        success: AcquisitionState,
    ) -> Result<(), HostError> {
        let mut registry = self.lock();
        let current = registry.jobs.get_mut(&job).expect("a committing job remains retained");
        match result {
            Ok(()) => {
                current.snapshot.revision = current.snapshot.revision.saturating_add(1);
                current.snapshot.state = success;
                let snapshot = current.snapshot.clone();
                self.events.acquisition(job, snapshot);
                Ok(())
            }
            Err(error) => {
                current.snapshot.revision = current.snapshot.revision.saturating_add(1);
                current.snapshot.state = AcquisitionState::Failed(error.to_string());
                let snapshot = current.snapshot.clone();
                self.events.acquisition(job, snapshot);
                Err(error)
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn snapshot(event: Acquisition) -> (AcquisitionState, Option<Candidate>) {
    match event {
        Acquisition::Inspecting => (AcquisitionState::Inspecting, None),
        Acquisition::Pulling {
            status,
            id,
            current,
            total,
        } => (
            AcquisitionState::Pulling {
                status,
                id,
                current,
                total,
            },
            None,
        ),
        Acquisition::ReadingManifest => (AcquisitionState::ReadingManifest, None),
        Acquisition::Ready(candidate) => {
            let visible = AcquisitionCandidate {
                reference: candidate.reference.clone(),
                digest: candidate.digest.clone(),
                name: candidate.manifest.name.to_string(),
                version: candidate.manifest.version.clone(),
                requested: candidate.manifest.capabilities.clone(),
            };
            (AcquisitionState::Ready(visible), Some(candidate))
        }
        Acquisition::Failed(reason) => (AcquisitionState::Failed(reason), None),
        Acquisition::Cancelled => (AcquisitionState::Cancelled, None),
    }
}

fn moment() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use hl_extension::{Capability, ExtensionName, Manifest, PROTOCOL};

    use super::*;

    fn manifest(version: &str, capabilities: &[Capability]) -> Manifest {
        Manifest {
            name: ExtensionName::new("sample").unwrap(),
            display_name: "Sample".into(),
            version: version.into(),
            protocol: PROTOCOL,
            capabilities: Grant::new(capabilities.iter().copied()),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    fn workspace(root: &std::path::Path) -> WorkspaceConfig {
        let mut workspace = WorkspaceConfig::new("test", "alpine", hl_ws::Arch::Amd64);
        workspace.storage = Some(root.to_owned());
        workspace
    }

    fn ready(service: &ExtensionAcquisitions, job: AcquisitionJob) -> AcquisitionSnapshot {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let snapshot = service.status(job).unwrap();
            if matches!(snapshot.state, AcquisitionState::Ready(_)) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "acquisition did not become ready");
            std::thread::yield_now();
        }
    }

    #[test]
    fn start_is_non_blocking_and_cancel_prevents_late_ready() {
        let root = tempfile::tempdir().unwrap();
        let service = ExtensionAcquisitions::with_acquirer(&workspace(root.path()), |_, _, progress, cancellation| {
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            let _ = progress.send(Acquisition::Ready(Candidate {
                reference: "late".into(),
                digest: "sha256:late".into(),
                manifest: manifest("1.0.0", &[]),
            }));
        });
        let started = Instant::now();
        let job = service.start("registry/sample:latest").unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        service.cancel(job).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(service.status(job).unwrap().state, AcquisitionState::Cancelled);
        assert!(service.install(job, 1, &Grant::default()).is_err());
    }

    #[test]
    fn install_persists_only_the_observed_digest_and_narrow_consent() {
        let root = tempfile::tempdir().unwrap();
        let observed = manifest("1.0.0", &[Capability::ContainerRead, Capability::ContainerControl]);
        let service =
            ExtensionAcquisitions::with_acquirer(&workspace(root.path()), move |_, reference, progress, _| {
                let _ = progress.send(Acquisition::Ready(Candidate {
                    reference: reference.into(),
                    digest: "sha256:observed".into(),
                    manifest: observed.clone(),
                }));
            });
        let job = service.start("registry/sample:1").unwrap();
        let snapshot = ready(&service, job);
        let AcquisitionState::Ready(candidate) = &snapshot.state else {
            unreachable!()
        };
        assert_eq!(candidate.digest, "sha256:observed");
        assert_eq!(candidate.name, "sample");
        assert_eq!(candidate.version, "1.0.0");
        assert!(candidate.requested.holds(Capability::ContainerControl));
        service
            .install(job, snapshot.revision, &Grant::new([Capability::ContainerRead]))
            .unwrap();
        assert_eq!(service.status(job).unwrap().state, AcquisitionState::Installed);
        assert!(
            service.install(job, snapshot.revision, &Grant::default()).is_err(),
            "a consent job is single use"
        );
        let entries = Roster::workspace(&workspace(root.path())).unwrap().entries();
        assert_eq!(entries[0].image_digest, "sha256:observed");
        assert!(entries[0].granted.holds(Capability::ContainerRead));
        assert!(!entries[0].granted.holds(Capability::ContainerControl));
    }

    #[test]
    fn update_commits_the_ready_revision_and_requires_fresh_consent() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        Roster::workspace(&workspace)
            .unwrap()
            .register(
                &manifest("1.0.0", &[Capability::ContainerRead]),
                "sha256:old",
                &Grant::new([Capability::ContainerRead]),
                1,
            )
            .unwrap();
        let next = manifest("2.0.0", &[Capability::ContainerRead, Capability::ContainerControl]);
        let service = ExtensionAcquisitions::with_acquirer(&workspace, move |_, reference, progress, _| {
            let _ = progress.send(Acquisition::Ready(Candidate {
                reference: reference.into(),
                digest: "sha256:new".into(),
                manifest: next.clone(),
            }));
        });
        let job = service.start("registry/sample:2").unwrap();
        let snapshot = ready(&service, job);
        assert!(
            service.update(job, snapshot.revision - 1, &Grant::default()).is_err(),
            "stale consent is refused"
        );
        service
            .update(
                job,
                snapshot.revision,
                &Grant::new([Capability::ContainerRead, Capability::ContainerControl]),
            )
            .unwrap();
        let entry = Roster::workspace(&workspace).unwrap().entries().remove(0);
        assert_eq!(
            (entry.version.as_str(), entry.image_digest.as_str()),
            ("2.0.0", "sha256:new")
        );
        assert!(entry.granted.holds(Capability::ContainerControl));
    }

    #[test]
    fn reference_and_active_job_bounds_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let service = ExtensionAcquisitions::with_acquirer(&workspace(root.path()), |_, _, _, cancellation| {
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
        });
        assert!(service.start("").is_err());
        assert!(service
            .start(&"x".repeat(ExtensionAcquisitions::REFERENCE_LIMIT + 1))
            .is_err());
        let jobs: Vec<_> = (0..ExtensionAcquisitions::ACTIVE_LIMIT)
            .map(|index| service.start(&format!("sample:{index}")).unwrap())
            .collect();
        assert!(service.start("sample:overflow").is_err());
        for job in jobs {
            service.cancel(job).unwrap();
        }
    }
}
