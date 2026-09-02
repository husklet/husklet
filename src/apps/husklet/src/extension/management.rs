//! Durable installed-extension inventory and lifecycle policy.

use hl_extension::port::{
    ExtensionAcquisitionJob, ExtensionAcquisitionProgress, ExtensionAcquisitionStatus, ExtensionCandidate,
    ExtensionStore, ExtensionSummary, HostError,
};
use hl_extension::{ExtensionName, Grant, Stage};
use hl_ws::storage::Directory;

use crate::config::WorkspaceConfig;

use super::acquisition::{AcquisitionJob, AcquisitionSnapshot, AcquisitionState, ExtensionAcquisitions};
use super::management_events::ExtensionEvents;
use super::Roster;

pub struct ExtensionManagement {
    workspace: WorkspaceConfig,
    acquisitions: ExtensionAcquisitions,
    events: ExtensionEvents,
}

impl ExtensionManagement {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        let events = ExtensionEvents::default();
        let management = Self {
            workspace: workspace.clone(),
            acquisitions: ExtensionAcquisitions::new(workspace, events.clone()),
            events,
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

    pub(crate) fn events(&self) -> ExtensionEvents {
        self.events.clone()
    }

    fn changed(&self, result: Result<(), HostError>) -> Result<(), HostError> {
        result?;
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(())
    }
}

impl ExtensionStore for ExtensionManagement {
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
        let result = self
            .roster()?
            .enable_if_digest(&Self::name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
    }

    fn disable(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let result = self
            .roster()?
            .disable_if_digest(&Self::name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
    }

    fn remove(&self, name: &str, image_digest: &str) -> Result<(), HostError> {
        let result = self
            .roster()?
            .remove_if_digest(&Self::name(name)?, image_digest)
            .map_err(failure);
        self.changed(result)
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

    fn acquisition_cancel(&self, job: &str) -> Result<(), HostError> {
        self.acquisitions.cancel(AcquisitionJob::parse(job)?)
    }

    fn install(&self, job: &str, revision: u64, granted: &Grant) -> Result<ExtensionSummary, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let name = ready_name(&self.acquisitions, job, revision)?;
        self.acquisitions.install(job, revision, granted)?;
        let installed = self.inspect(&name)?;
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(installed)
    }

    fn update(&self, job: &str, revision: u64, granted: &Grant) -> Result<ExtensionSummary, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let name = ready_name(&self.acquisitions, job, revision)?;
        self.acquisitions.update(job, revision, granted)?;
        let updated = self.inspect(&name)?;
        if let Ok(entries) = self.list() {
            self.events.inventory(entries);
        }
        Ok(updated)
    }
}

fn ready_name(service: &ExtensionAcquisitions, job: AcquisitionJob, revision: u64) -> Result<String, HostError> {
    let snapshot = service.status(job)?;
    if snapshot.revision != revision {
        return Err(HostError::Conflict("the acquisition revision has changed".into()));
    }
    match snapshot.state {
        AcquisitionState::Ready(candidate) => Ok(candidate.name),
        _ => Err(HostError::Conflict("the acquisition is not awaiting consent".into())),
    }
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
    ExtensionSummary {
        name: entry.name.to_string(),
        image_digest: entry.image_digest,
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

    fn workspace(root: &std::path::Path) -> WorkspaceConfig {
        let mut workspace = WorkspaceConfig::new("test", "alpine", hl_ws::Arch::Amd64);
        workspace.storage = Some(root.to_owned());
        workspace
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
    fn management_composes_initial_and_mutated_inventory_events() {
        let root = tempfile::tempdir().unwrap();
        let management = ExtensionManagement::new(&workspace(root.path()));
        let events = management.events();
        assert!(events.drain().unwrap().inventory.unwrap().is_empty());

        assert!(management.remove("absent", &format!("sha256:{}", "a".repeat(64))).is_err());
        assert!(events.drain().is_none());
    }
}
