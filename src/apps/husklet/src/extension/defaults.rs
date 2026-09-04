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
        concat!(
            "ghcr.io/husklet/husklet/extension-workspace:",
            env!("CARGO_PKG_VERSION")
        ),
    ),
    (
        "extensions",
        concat!(
            "ghcr.io/husklet/husklet/extension-extensions:",
            env!("CARGO_PKG_VERSION")
        ),
    ),
];

/// Acquires, grants, records, and enables the two trusted first-party surfaces.
///
/// Completed entries are retained when a later acquisition fails, so retrying
/// provisioning resumes instead of pulling and recording the same image again.
pub fn install_defaults(workspace: &WorkspaceConfig) -> Result<(), String> {
    install_defaults_with(workspace, Candidate::read)
}

fn install_defaults_with(
    workspace: &WorkspaceConfig,
    mut read: impl FnMut(&WorkspaceConfig, &str) -> Result<Candidate, String>,
) -> Result<(), String> {
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
        let candidate = read(workspace, reference)?;
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
    use hl_extension::{Activation, Capability, Grant, Manifest, Presentation, Resources};

    #[test]
    fn defaults_are_release_matched_and_sidebar_ordered() {
        assert_eq!(DEFAULT_EXTENSIONS[0].0, "workspace");
        assert_eq!(DEFAULT_EXTENSIONS[1].0, "extensions");
        for (name, reference) in DEFAULT_EXTENSIONS {
            assert_eq!(
                reference,
                format!("ghcr.io/husklet/husklet/extension-{name}:{}", env!("CARGO_PKG_VERSION"))
            );
        }
    }

    #[test]
    fn provisioning_records_exactly_the_two_enabled_extension_surfaces() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let mut acquired = Vec::new();

        install_defaults_with(&workspace, |_, reference| {
            acquired.push(reference.to_owned());
            let name = if reference.contains("extension-workspace:") {
                "workspace"
            } else if reference.contains("extension-extensions:") {
                "extensions"
            } else {
                panic!("unexpected default reference {reference}");
            };
            let capability = if name == "workspace" {
                Capability::WorkspaceRead
            } else {
                Capability::ExtensionRead
            };
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: format!("sha256:{name}"),
                manifest: Manifest {
                    name: ExtensionName::new(name).unwrap(),
                    display_name: name.to_owned(),
                    version: "0.1.0".to_owned(),
                    protocol: hl_extension::PROTOCOL,
                    capabilities: Grant::new([capability, Capability::Interface]),
                    entrypoint: None,
                    activation: Activation::Workspace,
                    interface: Some(Presentation {
                        tab_title: name.to_owned(),
                        icon: None,
                    }),
                    pane_providers: Vec::new(),
                    resources: Resources::default(),
                    filesystem_roots: Vec::new(),
                },
            })
        })
        .unwrap();

        assert_eq!(acquired, DEFAULT_EXTENSIONS.map(|(_, reference)| reference.to_owned()));
        let mut entries = Roster::workspace(&workspace).unwrap().entries();
        entries.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_str(), "extensions");
        assert_eq!(entries[0].image_digest, "sha256:extensions");
        assert_eq!(entries[0].stage, Stage::Duty);
        assert!(entries[0].granted.holds(Capability::ExtensionRead));
        assert_eq!(entries[1].name.as_str(), "workspace");
        assert_eq!(entries[1].image_digest, "sha256:workspace");
        assert_eq!(entries[1].stage, Stage::Duty);
        assert!(entries[1].granted.holds(Capability::WorkspaceRead));
    }
}
