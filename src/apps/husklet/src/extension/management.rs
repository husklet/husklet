//! Durable installed-extension inventory and lifecycle policy.

use hl_extension::port::{
    ExtensionAcquisitionJob, ExtensionAcquisitionProgress, ExtensionAcquisitionStatus, ExtensionCandidate,
    ExtensionCatalogue, ExtensionCatalogueEntry, ExtensionStore, ExtensionSummary, HostError,
};
use hl_extension::{ExtensionName, Grant, Stage};
use hl_ws::storage::Directory;

use crate::config::WorkspaceConfig;

use super::acquisition::{AcquisitionJob, AcquisitionSnapshot, AcquisitionState, ExtensionAcquisitions};
use super::management_events::ExtensionEvents;
use super::Roster;

trait RemovalCleanup {
    fn retire(&self) -> Result<(), HostError>;
    fn purge(self) -> Result<(), HostError>;
}

impl RemovalCleanup for super::host::ExtensionRemoval {
    fn retire(&self) -> Result<(), HostError> {
        super::host::ExtensionRemoval::retire(self)
    }

    fn purge(self) -> Result<(), HostError> {
        super::host::ExtensionRemoval::purge(self)
    }
}

pub struct ExtensionManagement {
    workspace: WorkspaceConfig,
    acquisitions: ExtensionAcquisitions,
    events: ExtensionEvents,
    lifecycle: std::sync::Mutex<()>,
}

impl ExtensionManagement {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        let events = ExtensionEvents::default();
        let management = Self {
            workspace: workspace.clone(),
            acquisitions: ExtensionAcquisitions::new(workspace, events.clone()),
            events,
            lifecycle: std::sync::Mutex::new(()),
        };
        if let Ok(entries) = management.list() {
            management.events.inventory(entries);
        }
        management
    }

    fn roster(&self) -> Result<Roster<Directory>, HostError> {
        Roster::workspace(&self.workspace).map_err(failure)
    }

    fn name(value: &str) -> Result<ExtensionName, HostError> {
        ExtensionName::new(value).map_err(|error| HostError::Conflict(error.to_string()))
    }

    fn optional_name(value: &str) -> Result<ExtensionName, HostError> {
        let name = Self::name(value)?;
        if super::defaults::DEFAULT_EXTENSIONS
            .iter()
            .any(|(required, _)| name.as_str() == *required)
        {
            return Err(HostError::Conflict(
                "Top is the required workspace management extension and cannot be disabled or removed".into(),
            ));
        }
        Ok(name)
    }

    pub(crate) fn events(&self) -> ExtensionEvents {
        self.events.clone()
    }

    fn changed(&self, result: Result<(), HostError>) -> Result<(), HostError> {
        result?;
        super::revision::publish_inventory_change(&self.workspace);
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(())
    }

    fn remove_with<R: RemovalCleanup>(
        &self,
        name: ExtensionName,
        image_digest: &str,
        cleanup: R,
    ) -> Result<(), HostError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        // Runtime cleanup can fail transiently. Keep the exact digest-bound
        // record until it succeeds so the same uninstall remains retryable.
        cleanup.retire()?;
        let mut roster = self.roster()?;
        let removed = roster.take_if_digest(&name, image_digest).map_err(failure)?;
        let cleanup_result = cleanup.purge().and_then(|()| {
            super::StateBlob::new(&self.workspace.storage_dir(&crate::paths::hl_root()), &name)
            .map_err(|error| HostError::Failed(error.to_string()))?
            .purge()
        });
        if let Err(error) = cleanup_result {
            roster.restore(&removed).map_err(|rollback| {
                HostError::Failed(format!("{error}; restoring extension authority also failed: {rollback}"))
            })?;
            return Err(error);
        }
        self.changed(Ok(()))
    }
}

impl ExtensionStore for ExtensionManagement {
    fn catalogue(&self) -> Result<ExtensionCatalogue, HostError> {
        Ok(ExtensionCatalogue {
            entries: first_party_catalogue(option_env!("HL_STORYBOOK_IMAGE"), self.workspace.arch.as_str()),
            complete: true,
        })
    }

    fn list(&self) -> Result<Vec<ExtensionSummary>, HostError> {
        Ok(self.roster()?.entries().into_iter().map(summary).collect())
    }

    fn inspect(&self, name: &str) -> Result<ExtensionSummary, HostError> {
        let name = Self::name(name)?;
        self.roster()?
            .entries()
            .into_iter()
            .find(|entry| entry.name == name)
            .map(summary)
            .ok_or_else(|| HostError::Absent(name.to_string()))
    }

    fn enable(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        let result = self
            .roster()?
            .enable_if_digest(&Self::name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
    }

    fn disable(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        let result = self
            .roster()?
            .disable_if_digest(&Self::optional_name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
    }

    fn retry(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        let result = self
            .roster()?
            .retry_if_digest(&Self::name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
    }

    fn remove(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let name = Self::optional_name(name)?;
        // Capture exact runtime ownership before forgetting its record. Cleanup
        // runs only after the digest-bound record mutation succeeds, so a
        // storage failure cannot leave an installed extension with erased data.
        let removal = super::Workspace::extension_removal(&self.workspace, &name, image_digest)?;
        self.remove_with(name, image_digest, removal)
    }

    fn acquisition_start(&self, reference: &str) -> Result<ExtensionAcquisitionJob, HostError> {
        Ok(ExtensionAcquisitionJob {
            job: self.acquisitions.start(reference)?.wire(),
        })
    }

    fn acquisition_status(&self, job: &str) -> Result<ExtensionAcquisitionStatus, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let snapshot = self.acquisitions.status(job)?;
        Ok(acquisition_status(job.wire(), snapshot))
    }

    fn acquisition_cancel(&self, job: &str, revision: u64) -> Result<(), HostError> {
        self.acquisitions.cancel(AcquisitionJob::parse(job)?, revision)
    }

    fn install(
        &self,
        job: &str,
        revision: u64,
        image_digest: &str,
        granted: &Grant,
        containers: &hl_extension::ContainerGrant,
        images: &hl_extension::ImageGrant,
        networks: &hl_extension::NetworkGrant,
        volumes: &hl_extension::VolumeGrant,
        filesystem: &hl_extension::FilesystemGrant,
        workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
    ) -> Result<ExtensionSummary, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let name = ready_name(&self.acquisitions, job, revision, image_digest)?;
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        self.acquisitions.install_resource_scoped(
            job,
            revision,
            granted,
            containers,
            images,
            networks,
            volumes,
            filesystem,
            workspace_environment,
        )?;
        super::revision::publish_inventory_change(&self.workspace);
        let installed = self.inspect(&name)?;
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(installed)
    }

    fn update(
        &self,
        job: &str,
        revision: u64,
        image_digest: &str,
        granted: &Grant,
        containers: &hl_extension::ContainerGrant,
        images: &hl_extension::ImageGrant,
        networks: &hl_extension::NetworkGrant,
        volumes: &hl_extension::VolumeGrant,
        filesystem: &hl_extension::FilesystemGrant,
        workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
    ) -> Result<ExtensionSummary, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let name = ready_name(&self.acquisitions, job, revision, image_digest)?;
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| HostError::Failed("extension lifecycle lock is poisoned".into()))?;
        self.acquisitions.update_resource_scoped(
            job,
            revision,
            granted,
            containers,
            images,
            networks,
            volumes,
            filesystem,
            workspace_environment,
        )?;
        super::revision::publish_inventory_change(&self.workspace);
        let updated = self.inspect(&name)?;
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(updated)
    }
}

fn ready_name(
    service: &ExtensionAcquisitions,
    job: AcquisitionJob,
    revision: u64,
    image_digest: &str,
) -> Result<String, HostError> {
    let snapshot = service.status(job)?;
    reviewed_name(snapshot, revision, image_digest)
}

fn reviewed_name(snapshot: AcquisitionSnapshot, revision: u64, image_digest: &str) -> Result<String, HostError> {
    if snapshot.revision != revision {
        return Err(HostError::Conflict("the acquisition revision has changed".into()));
    }
    match snapshot.state {
        AcquisitionState::Ready(candidate) if candidate.digest == image_digest => Ok(candidate.name),
        AcquisitionState::Ready(_) => Err(HostError::Conflict(
            "the acquisition candidate digest differs from the reviewed image digest".into(),
        )),
        _ => Err(HostError::Conflict("the acquisition is not awaiting consent".into())),
    }
}

fn first_party_catalogue(reference: Option<&str>, architecture: &str) -> Vec<ExtensionCatalogueEntry> {
    let reference = reference.unwrap_or(concat!(
        "ghcr.io/husklet/husklet/extension-storybook:",
        env!("CARGO_PKG_VERSION")
    ));
    std::iter::once(ExtensionCatalogueEntry {
        id: "storybook".into(),
        title: "Component playground".into(),
        description: "Explore extension components, large tables, terminals, diffs, and metrics.".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        reference: reference.into(),
        publisher: "Husklet".into(),
        source: "husklet:first-party/storybook".into(),
        publisher_verified: true,
        protocol: hl_extension::PROTOCOL,
        architectures: vec![architecture.into()],
    })
    .collect()
}

fn acquisition_status(job: String, snapshot: AcquisitionSnapshot) -> ExtensionAcquisitionStatus {
    let reference = snapshot.reference;
    let (state, progress, candidate, error) = match snapshot.state {
        AcquisitionState::Inspecting => ("inspecting", None, None, None),
        AcquisitionState::Pulling {
            status,
            id,
            current,
            total,
        } => (
            "pulling",
            Some(ExtensionAcquisitionProgress {
                status,
                id,
                current,
                total,
            }),
            None,
            None,
        ),
        AcquisitionState::ReadingManifest => ("reading-manifest", None, None, None),
        AcquisitionState::Ready(candidate) => {
            let candidate = ExtensionCandidate {
                name: ExtensionName::new(&candidate.name).expect("acquired manifests have valid names"),
                version: candidate.version,
                image_digest: candidate.digest,
                requested: candidate.requested,
                required: candidate.required,
                requested_images: candidate.requested_images,
                requested_containers: candidate.requested_containers,
                requested_networks: candidate.requested_networks,
                requested_volumes: candidate.requested_volumes,
                requested_filesystem: candidate.requested_filesystem,
                requested_workspace_environment: candidate.requested_workspace_environment,
                installed_image_digest: candidate.installed_digest,
            };
            ("ready", None, Some(candidate), None)
        }
        AcquisitionState::Committing => ("committing", None, None, None),
        AcquisitionState::Installed => ("installed", None, None, None),
        AcquisitionState::Updated => ("updated", None, None, None),
        AcquisitionState::Failed(error) => ("failed", None, None, Some(error)),
        AcquisitionState::Cancelled => ("cancelled", None, None, None),
    };
    ExtensionAcquisitionStatus {
        job,
        reference,
        revision: snapshot.revision,
        state: state.into(),
        progress,
        candidate,
        error,
    }
}

fn summary(entry: super::roster::Entry) -> ExtensionSummary {
    let enabled = !matches!(entry.stage, Stage::Standby | Stage::Vacancy);
    ExtensionSummary {
        name: entry.name.to_string(),
        image_digest: entry.image_digest,
        version: entry.version,
        enabled,
        pane_providers: entry.pane_providers,
        granted: entry.granted,
        images: entry.images,
        containers: entry.containers,
        networks: entry.networks,
        volumes: entry.volumes,
        filesystem: entry.filesystem,
        workspace_environment: entry.workspace_environment,
        status: match entry.stage {
            Stage::Vacancy => "vacancy".into(),
            Stage::Standby => "standby".into(),
            Stage::Duty => "duty".into(),
            Stage::Fault { restarts } => format!("fault:{restarts}"),
        },
    }
}

fn failure(error: super::Refusal) -> HostError {
    HostError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_extension::port::ExtensionStateStore as _;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    struct Cleanup {
        data: std::path::PathBuf,
        fail_retire: bool,
        fail_purge: bool,
    }

    impl RemovalCleanup for Cleanup {
        fn retire(&self) -> Result<(), HostError> {
            if self.fail_retire {
                Err(HostError::Failed("sidecar removal timed out".into()))
            } else {
                Ok(())
            }
        }

        fn purge(self) -> Result<(), HostError> {
            if self.fail_purge {
                return Err(HostError::Failed("sidecar data purge timed out".into()));
            }
            std::fs::remove_dir_all(self.data).map_err(|error| HostError::Failed(error.to_string()))
        }
    }

    struct BlockingPurge {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl RemovalCleanup for BlockingPurge {
        fn retire(&self) -> Result<(), HostError> {
            Ok(())
        }

        fn purge(self) -> Result<(), HostError> {
            self.entered.send(()).expect("announce purge");
            self.release.recv().expect("release purge");
            Err(HostError::Failed("injected purge failure".into()))
        }
    }

    fn workspace(root: &std::path::Path) -> WorkspaceConfig {
        let mut workspace = WorkspaceConfig::new("test", "alpine", hl_ws::Arch::Amd64);
        workspace.storage = Some(root.to_owned());
        workspace
    }

    #[test]
    fn lifecycle_mutation_cannot_occupy_a_name_during_uninstall_rollback() {
        let root = tempfile::tempdir().unwrap();
        let management = Arc::new(ExtensionManagement::new(&workspace(root.path())));
        let name = ExtensionName::new("postgres").unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let manifest = hl_extension::Manifest {
            name: name.clone(),
            display_name: "Postgres".into(),
            version: "1".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::default(),
            activation: hl_extension::Activation::default(),
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            entrypoint: None,
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        management
            .roster()
            .unwrap()
            .register(&manifest, &digest, &manifest.capabilities, 1)
            .unwrap();
        let (entered, entered_receiver) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let removing = Arc::clone(&management);
        let removed_name = name.clone();
        let removed_digest = digest.clone();
        let removal = std::thread::spawn(move || {
            removing.remove_with(
                removed_name,
                &removed_digest,
                BlockingPurge {
                    entered,
                    release: release_receiver,
                },
            )
        });
        entered_receiver.recv().expect("uninstall reached purge");

        let enabling = Arc::clone(&management);
        let enabled_digest = digest.clone();
        let (finished, finished_receiver) = mpsc::channel();
        let enable = std::thread::spawn(move || {
            let result = enabling.enable("postgres", &enabled_digest);
            finished.send(()).expect("announce lifecycle completion");
            result
        });
        assert!(
            finished_receiver.recv_timeout(Duration::from_millis(100)).is_err(),
            "another lifecycle mutation entered the uninstall rollback window"
        );
        release.send(()).expect("release uninstall");
        assert!(removal.join().expect("removal thread").is_err());
        enable.join().expect("enable thread").expect("restored extension enabled");
        assert_eq!(management.inspect("postgres").unwrap().image_digest, digest);
    }

    #[test]
    fn catalogue_advertises_only_a_release_proven_reference() {
        let root = tempfile::tempdir().unwrap();
        let catalogue = ExtensionManagement::new(&workspace(root.path())).catalogue().unwrap();
        assert!(catalogue.complete);
        assert_eq!(
            catalogue.entries,
            first_party_catalogue(option_env!("HL_STORYBOOK_IMAGE"), "amd64")
        );

        let entries = first_party_catalogue(Some("registry.example/husklet/storybook:4"), "arm64");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].protocol, hl_extension::PROTOCOL);
        assert_eq!(entries[0].architectures, ["arm64"]);
        assert_eq!(entries[0].reference, "registry.example/husklet/storybook:4");
        assert_eq!(
            first_party_catalogue(None, "amd64")[0].reference,
            format!(
                "ghcr.io/husklet/husklet/extension-storybook:{}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn acquisition_status_preserves_real_registry_progress() {
        let status = acquisition_status(
            "9".into(),
            AcquisitionSnapshot {
                reference: "registry.example/team/tool:1".into(),
                revision: 4,
                state: AcquisitionState::Pulling {
                    status: "downloading layer".into(),
                    id: Some("sha256:layer".into()),
                    current: Some(25),
                    total: Some(100),
                },
            },
        );
        assert_eq!(
            (status.job.as_str(), status.reference.as_str(), status.revision),
            ("9", "registry.example/team/tool:1", 4)
        );
        let progress = status.progress.expect("pull progress remains structured");
        assert_eq!(
            (progress.status.as_str(), progress.current, progress.total),
            ("downloading layer", Some(25), Some(100))
        );
    }

    #[test]
    fn ready_status_exposes_the_installed_generation_that_consent_is_bound_to() {
        let status = acquisition_status(
            "9".into(),
            AcquisitionSnapshot {
                reference: "registry.example/team/tool:2".into(),
                revision: 7,
                state: AcquisitionState::Ready(crate::extension::acquisition::AcquisitionCandidate {
                    requested_images: hl_extension::ImageGrant::default(),
                    requested_containers: hl_extension::ContainerGrant::default(),
                    requested_networks: hl_extension::NetworkGrant::default(),
                    requested_volumes: hl_extension::VolumeGrant::default(),
                    requested_filesystem: hl_extension::FilesystemGrant::default(),
                    requested_workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
                    reference: "registry.example/team/tool:2".into(),
                    digest: "sha256:new".into(),
                    name: "sample".into(),
                    version: "2".into(),
                    requested: Grant::new([hl_extension::Capability::Interface]),
                    required: Grant::new([hl_extension::Capability::Interface]),
                    installed_digest: Some("sha256:old".into()),
                }),
            },
        );
        assert_eq!(
            status.candidate.unwrap().installed_image_digest.as_deref(),
            Some("sha256:old")
        );
    }

    #[test]
    fn consent_commit_requires_the_exact_reviewed_candidate_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let snapshot = AcquisitionSnapshot {
            reference: "registry.example/team/tool:latest".into(),
            revision: 7,
            state: AcquisitionState::Ready(crate::extension::acquisition::AcquisitionCandidate {
                requested_images: hl_extension::ImageGrant::default(),
                requested_containers: hl_extension::ContainerGrant::default(),
                requested_networks: hl_extension::NetworkGrant::default(),
                requested_volumes: hl_extension::VolumeGrant::default(),
                requested_filesystem: hl_extension::FilesystemGrant::default(),
                requested_workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
                reference: "registry.example/team/tool:latest".into(),
                digest: digest.clone(),
                name: "sample".into(),
                version: "2".into(),
                requested: Grant::default(),
                required: Grant::default(),
                installed_digest: None,
            }),
        };
        assert_eq!(reviewed_name(snapshot.clone(), 7, &digest), Ok("sample".into()));
        assert!(matches!(
            reviewed_name(snapshot, 7, &format!("sha256:{}", "b".repeat(64))),
            Err(HostError::Conflict(reason)) if reason.contains("reviewed image digest")
        ));
    }

    #[test]
    fn management_composes_initial_and_mutated_inventory_events() {
        let root = tempfile::tempdir().unwrap();
        let management = ExtensionManagement::new(&workspace(root.path()));
        let events = management.events();
        assert!(events.drain().unwrap().inventory.unwrap().is_empty());

        assert!(management
            .remove("absent", &format!("sha256:{}", "a".repeat(64)))
            .is_err());
        assert!(events.drain().is_none());
    }

    #[test]
    fn failed_removal_retains_private_extension_state() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let management = ExtensionManagement::new(&workspace);
        let name = ExtensionName::new("postgres").unwrap();
        let state = super::super::StateBlob::new(root.path(), &name).unwrap();
        state.write("absent", b"migration-checkpoint").unwrap();
        let data = root.path().join("extensions/postgres/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("index.db"), b"keep").unwrap();

        assert!(management
            .remove(name.as_str(), &format!("sha256:{}", "a".repeat(64)))
            .is_err());
        assert_eq!(state.read().unwrap().contents, b"migration-checkpoint");
        assert_eq!(std::fs::read(data.join("index.db")).unwrap(), b"keep");
    }

    #[test]
    fn successful_removal_clears_private_extension_state() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let management = ExtensionManagement::new(&workspace);
        let name = ExtensionName::new("postgres").unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let manifest = hl_extension::Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: name.clone(),
            display_name: "Postgres".into(),
            version: "1".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new([hl_extension::Capability::StateWrite]),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        management
            .roster()
            .unwrap()
            .register(&manifest, &digest, &manifest.capabilities, 1)
            .unwrap();
        let state = super::super::StateBlob::new(root.path(), &name).unwrap();
        state.write("absent", b"remove-me").unwrap();
        let data = root.path().join("extensions/postgres/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("index.db"), b"remove-me-too").unwrap();

        management.remove(name.as_str(), &digest).unwrap();
        assert!(state.read().unwrap().contents.is_empty());
        assert!(!data.exists(), "uninstall must remove the extension's private data");
    }

    #[test]
    fn sidecar_cleanup_failure_retains_exact_authority_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let management = ExtensionManagement::new(&workspace);
        let name = ExtensionName::new("postgres").unwrap();
        let digest = format!("sha256:{}", "c".repeat(64));
        let manifest = hl_extension::Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: name.clone(),
            display_name: "Postgres".into(),
            version: "1".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new([hl_extension::Capability::StateWrite]),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        management
            .roster()
            .unwrap()
            .register(&manifest, &digest, &manifest.capabilities, 1)
            .unwrap();
        let state = super::super::StateBlob::new(root.path(), &name).unwrap();
        state.write("absent", b"retry-state").unwrap();
        let data = root.path().join("extensions/postgres/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("index.db"), b"retry-data").unwrap();

        let failure = management
            .remove_with(
                name.clone(),
                &digest,
                Cleanup {
                    data: data.clone(),
                    fail_retire: true,
                    fail_purge: false,
                },
            )
            .expect_err("runtime cleanup fails");
        assert!(matches!(failure, HostError::Failed(reason) if reason.contains("timed out")));
        assert_eq!(management.inspect(name.as_str()).unwrap().image_digest, digest);
        assert_eq!(state.read().unwrap().contents, b"retry-state");
        assert_eq!(std::fs::read(data.join("index.db")).unwrap(), b"retry-data");

        let failure = management
            .remove_with(
                name.clone(),
                &digest,
                Cleanup {
                    data: data.clone(),
                    fail_retire: false,
                    fail_purge: true,
                },
            )
            .expect_err("private data cleanup fails");
        assert!(matches!(failure, HostError::Failed(reason) if reason.contains("data purge timed out")));
        assert_eq!(management.inspect(name.as_str()).unwrap().image_digest, digest);
        assert_eq!(state.read().unwrap().contents, b"retry-state");
        assert_eq!(std::fs::read(data.join("index.db")).unwrap(), b"retry-data");

        management
            .remove_with(
                name.clone(),
                &digest,
                Cleanup {
                    data: data.clone(),
                    fail_retire: false,
                    fail_purge: false,
                },
            )
            .unwrap();
        assert!(matches!(management.inspect(name.as_str()), Err(HostError::Absent(_))));
        assert!(state.read().unwrap().contents.is_empty());
        assert!(!data.exists());
    }

    #[test]
    fn stale_uninstall_cannot_purge_the_newer_extensions_private_data() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let management = ExtensionManagement::new(&workspace);
        let name = ExtensionName::new("postgres").unwrap();
        let digest = format!("sha256:{}", "b".repeat(64));
        let manifest = hl_extension::Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: name.clone(),
            display_name: "Postgres".into(),
            version: "2".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::default(),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        management
            .roster()
            .unwrap()
            .register(&manifest, &digest, &manifest.capabilities, 2)
            .unwrap();
        let data = root.path().join("extensions/postgres/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("index.db"), b"new generation").unwrap();

        assert!(matches!(
            management.remove(name.as_str(), &format!("sha256:{}", "a".repeat(64))),
            Err(HostError::Conflict(_))
        ));
        assert_eq!(std::fs::read(data.join("index.db")).unwrap(), b"new generation");
        assert_eq!(management.inspect(name.as_str()).unwrap().image_digest, digest);
    }

    #[test]
    fn disabling_an_extension_preserves_its_private_data() {
        let root = tempfile::tempdir().unwrap();
        let workspace = workspace(root.path());
        let management = ExtensionManagement::new(&workspace);
        let name = ExtensionName::new("postgres").unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let manifest = hl_extension::Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            name: name.clone(),
            display_name: "Postgres".into(),
            version: "1".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::default(),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: hl_extension::Resources::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        let mut roster = management.roster().unwrap();
        roster.register(&manifest, &digest, &manifest.capabilities, 1).unwrap();
        roster.enable_if_digest(&name, &digest).unwrap();
        let data = root.path().join("extensions/postgres/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("index.db"), b"retained").unwrap();

        management.disable(name.as_str(), &digest).unwrap();

        assert_eq!(std::fs::read(data.join("index.db")).unwrap(), b"retained");
    }

    #[test]
    fn socket_management_cannot_remove_its_only_recovery_extension() {
        let root = tempfile::tempdir().unwrap();
        let management = ExtensionManagement::new(&workspace(root.path()));
        let digest = format!("sha256:{}", "a".repeat(64));

        for result in [management.disable("top", &digest), management.remove("top", &digest)] {
            assert!(
                matches!(result, Err(HostError::Conflict(reason)) if reason.contains("required workspace management"))
            );
        }
    }

    #[test]
    fn summary_exposes_enabled_digest_bound_provider_declarations() {
        let provider = hl_extension::PaneProvider {
            id: ExtensionName::new("main").unwrap(),
            title: "Top".into(),
            icon: Some("applications-system-symbolic".into()),
        };
        let value = summary(super::super::roster::Entry {
            name: ExtensionName::new("top").unwrap(),
            display_name: "Top".into(),
            interface: None,
            image_digest: format!("sha256:{}", "d".repeat(64)),
            version: "2.1.0".into(),
            granted: Grant::new([hl_extension::Capability::Interface]),
            images: hl_extension::ImageGrant::default(),
            containers: hl_extension::ContainerGrant {
                selectors: vec![hl_extension::ContainerSelector::Name {
                    name: "database".into(),
                }],
                create: false,
            },
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant {
                read: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                    workspace: "dev".into(),
                    name: "PGPASSWORD".into(),
                }],
                write: Vec::new(),
            },
            filesystem: hl_extension::FilesystemGrant {
                write: vec![hl_extension::FilesystemSelector::Exact {
                    exact: hl_extension::RelativePath::new("settings.json").unwrap(),
                }],
                ..hl_extension::FilesystemGrant::default()
            },
            stage: Stage::Duty,
            pane_providers: vec![provider.clone()],
        });
        assert!(value.enabled);
        assert_eq!(value.version, "2.1.0");
        assert!(value.granted.holds(hl_extension::Capability::Interface));
        assert_eq!(value.containers.selectors.len(), 1);
        assert!(matches!(
            &value.workspace_environment.read[0],
            hl_extension::WorkspaceEnvironmentSelector::Exact { workspace, name }
                if workspace == "dev" && name == "PGPASSWORD"
        ));
        assert_eq!(value.pane_providers, vec![provider]);
        assert_eq!(
            value.filesystem.write,
            vec![hl_extension::FilesystemSelector::Exact {
                exact: hl_extension::RelativePath::new("settings.json").unwrap()
            }]
        );
    }
}
