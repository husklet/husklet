//! Durable installed-extension inventory and lifecycle policy.

use hl_extension::port::{ExtensionStore, ExtensionSummary, HostError};
use hl_extension::{ExtensionName, Stage};
use hl_ws::storage::Directory;

use crate::config::WorkspaceConfig;

use super::Roster;

pub struct ExtensionManagement {
    workspace: WorkspaceConfig,
}

impl ExtensionManagement {
    pub fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            workspace: workspace.clone(),
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
