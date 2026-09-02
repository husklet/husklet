//! Durable installed-extension inventory and lifecycle policy.

use hl_extension::port::{
    ExtensionAcquisitionJob, ExtensionAcquisitionProgress, ExtensionAcquisitionStatus, ExtensionCandidate,
    ExtensionStore, ExtensionSummary, HostError,
};
use hl_extension::{ExtensionName, Grant, Stage};
use hl_ws::storage::Directory;

use crate::config::WorkspaceConfig;

use super::Roster;
use super::acquisition::{AcquisitionJob, AcquisitionSnapshot, AcquisitionState, ExtensionAcquisitions};

pub struct ExtensionManagement {
    workspace: WorkspaceConfig,
    acquisitions: ExtensionAcquisitions,
}

impl ExtensionManagement {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            workspace: workspace.clone(),
            acquisitions: ExtensionAcquisitions::new(workspace),
        }
    }

    fn roster(&self) -> Result<Roster<Directory>, HostError> {
        Roster::workspace(&self.workspace).map_err(failure)
    }

    fn name(value: &str) -> Result<ExtensionName, HostError> {
        ExtensionName::new(value).map_err(|error| HostError::Conflict(error.to_string()))
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

    fn enable(&self, name: &str) -> Result<(), HostError> {
        self.roster()?.enable(&Self::name(name)?).map_err(failure)
    }

    fn disable(&self, name: &str) -> Result<(), HostError> {
        self.roster()?.disable(&Self::name(name)?).map_err(failure)
    }

    fn remove(&self, name: &str) -> Result<(), HostError> {
        self.roster()?.remove(&Self::name(name)?).map_err(failure)
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
        self.inspect(&name)
    }

    fn update(&self, job: &str, revision: u64, granted: &Grant) -> Result<ExtensionSummary, HostError> {
        let job = AcquisitionJob::parse(job)?;
        let name = ready_name(&self.acquisitions, job, revision)?;
        self.acquisitions.update(job, revision, granted)?;
        self.inspect(&name)
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
}
