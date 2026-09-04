//! First-party extensions installed while a workspace is provisioned.

use hl_extension::{ExtensionName, Stage};

use super::{Candidate, Roster};
use crate::config::WorkspaceConfig;

/// Ordered identities and release-matched image references for a new workspace.
///
/// This order is also their default order in the workspace sidebar.
pub const DEFAULT_EXTENSIONS: [(&str, &str); 2] = [
    (
        "workspace",
        concat!("ghcr.io/husklet/husklet/extension-workspace:", env!("CARGO_PKG_VERSION")),
    ),
    (
        "extensions",
        concat!("ghcr.io/husklet/husklet/extension-extensions:", env!("CARGO_PKG_VERSION")),
    ),
];

/// Acquires, grants, records, and enables the two trusted first-party surfaces.
///
/// Completed entries are retained when a later acquisition fails, so retrying
/// provisioning resumes instead of pulling and recording the same image again.
pub fn install_defaults(workspace: &WorkspaceConfig) -> Result<(), String> {
    let mut roster = Roster::workspace(workspace).map_err(|error| error.to_string())?;
    for (expected, reference) in DEFAULT_EXTENSIONS {
        let name = ExtensionName::new(expected).map_err(|error| error.to_string())?;
        if let Some(entry) = roster.entries().into_iter().find(|entry| entry.name == name) {
            match entry.stage {
                Stage::Duty => continue,
                Stage::Standby => roster.enable(&name).map_err(|error| error.to_string())?,
                Stage::Fault { .. } => roster.retry(&name).map_err(|error| error.to_string())?,
                Stage::Vacancy => unreachable!("entries never contain vacancies"),
            }
            continue;
        }
        let candidate = Candidate::read(workspace, reference)?;
        if candidate.manifest.name != name {
            return Err(format!(
                "{reference} declares extension {}, expected {expected}",
                candidate.manifest.name
            ));
        }
        roster
            .register(
                &candidate.manifest,
                &candidate.digest,
                &candidate.manifest.capabilities,
                moment(),
            )
            .map_err(|error| error.to_string())?;
        roster.enable(&name).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn moment() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_release_matched_and_sidebar_ordered() {
        assert_eq!(DEFAULT_EXTENSIONS[0].0, "workspace");
        assert_eq!(DEFAULT_EXTENSIONS[1].0, "extensions");
        for (name, reference) in DEFAULT_EXTENSIONS {
            assert_eq!(
                reference,
                format!(
                    "ghcr.io/husklet/husklet/extension-{name}:{}",
                    env!("CARGO_PKG_VERSION")
                )
            );
        }
    }
}
