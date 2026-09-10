//! First-party extensions installed while a workspace is provisioned.

use hl_extension::{ExtensionName, Stage};

use super::{Candidate, Roster};
use crate::config::WorkspaceConfig;

/// Ordered identities and release-matched image references for a new workspace.
///
/// This order is also their default order in the workspace sidebar.
pub const DEFAULT_EXTENSIONS: [(&str, &str); 1] = [(
    "top",
    concat!("ghcr.io/husklet/husklet/extension-top:", env!("CARGO_PKG_VERSION")),
)];

/// Acquires, grants, records, and enables the trusted first-party control surface.
///
/// A retry inspects the release reference again before trusting retained state.
/// Tags are human release coordinates rather than immutable identity: a failed
/// provisioning attempt may have left a record from an earlier image carrying
/// the same tag, and the current digest must replace it before the workspace is
/// allowed to start.
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
        let candidate = read(workspace, reference).map_err(|reason| {
            format!(
                "default extension {expected} {version} is unavailable from public image {reference}: {reason}; check registry access, then retry workspace provisioning",
                version = env!("CARGO_PKG_VERSION")
            )
        })?;
        if candidate.manifest.name != name {
            return Err(format!(
                "default extension {expected} {version} could not be installed from public image {reference}: the image declares extension {}, expected {expected}; verify the image publisher and manifest, then retry workspace provisioning",
                candidate.manifest.name,
                version = env!("CARGO_PKG_VERSION")
            ));
        }
        if let Some(entry) = roster.entries().into_iter().find(|entry| entry.name == name) {
            if entry.image_digest != candidate.digest {
                let update = roster
                    .prepare_update(&candidate.manifest, &candidate.digest)
                    .map_err(|error| error.to_string())?;
                roster
                    .commit_update_resource_scoped(
                        update,
                        &candidate.manifest.capabilities,
                        &candidate.manifest.containers,
                        &candidate.manifest.images,
                        &candidate.manifest.networks,
                        &candidate.manifest.volumes,
                        &candidate.manifest.filesystem,
                        &candidate.manifest.workspace_environment,
                        moment(),
                    )
                    .map_err(|error| error.to_string())?;
            }
            match roster.stage(&name) {
                Stage::Duty => {}
                Stage::Standby => roster.enable(&name).map_err(|error| error.to_string())?,
                Stage::Fault { .. } => roster.retry(&name).map_err(|error| error.to_string())?,
                Stage::Vacancy => unreachable!("the existing default remains installed"),
            }
        } else {
            roster
                .register_resource_scoped(
                    &candidate.manifest,
                    &candidate.digest,
                    &candidate.manifest.capabilities,
                    &candidate.manifest.containers,
                    &candidate.manifest.images,
                    &candidate.manifest.networks,
                    &candidate.manifest.volumes,
                    &candidate.manifest.filesystem,
                    &candidate.manifest.workspace_environment,
                    moment(),
                )
                .map_err(|error| error.to_string())?;
            roster.enable(&name).map_err(|error| error.to_string())?;
        }
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
        assert_eq!(DEFAULT_EXTENSIONS[0].0, "top");
        for (name, reference) in DEFAULT_EXTENSIONS {
            assert_eq!(
                reference,
                format!("ghcr.io/husklet/husklet/extension-{name}:{}", env!("CARGO_PKG_VERSION"))
            );
        }
    }

    #[test]
    fn provisioning_records_the_enabled_top_surface() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let mut acquired = Vec::new();

        install_defaults_with(&workspace, |_, reference| {
            acquired.push(reference.to_owned());
            let name = if reference.contains("extension-top:") {
                "top"
            } else {
                panic!("unexpected default reference {reference}");
            };
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: format!("sha256:{name}"),
                manifest: Manifest {
                    containers: hl_extension::ContainerGrant::default(),
                    images: hl_extension::ImageGrant::default(),
                    networks: hl_extension::NetworkGrant::default(),
                    volumes: hl_extension::VolumeGrant::default(),
                    name: ExtensionName::new(name).unwrap(),
                    display_name: name.to_owned(),
                    version: "0.1.0".to_owned(),
                    protocol: hl_extension::PROTOCOL,
                    capabilities: Grant::new([
                        Capability::WorkspaceRead,
                        Capability::WorkspaceEnvironmentRead,
                        Capability::WorkspaceEnvironmentWrite,
                        Capability::ExtensionRead,
                        Capability::Interface,
                    ]),
                    entrypoint: None,
                    activation: Activation::Workspace,
                    interface: Some(Presentation {
                        tab_title: name.to_owned(),
                        icon: None,
                    }),
                    pane_providers: Vec::new(),
                    resources: Resources::default(),
                    filesystem: hl_extension::FilesystemGrant::default(),
                    workspace_environment: hl_extension::WorkspaceEnvironmentGrant {
                        read: vec![hl_extension::WorkspaceEnvironmentSelector::All { all: true }],
                        write: vec![hl_extension::WorkspaceEnvironmentSelector::All { all: true }],
                    },
                },
            })
        })
        .unwrap();

        assert_eq!(acquired, DEFAULT_EXTENSIONS.map(|(_, reference)| reference.to_owned()));
        let mut entries = Roster::workspace(&workspace).unwrap().entries();
        entries.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.as_str(), "top");
        assert_eq!(entries[0].image_digest, "sha256:top");
        assert_eq!(
            entries[0].workspace_environment.read,
            vec![hl_extension::WorkspaceEnvironmentSelector::All { all: true }]
        );
        assert_eq!(
            entries[0].workspace_environment.write,
            vec![hl_extension::WorkspaceEnvironmentSelector::All { all: true }]
        );
        assert_eq!(entries[0].stage, Stage::Duty);
        assert!(entries[0].granted.holds(Capability::ExtensionRead));
        assert!(entries[0].granted.holds(Capability::WorkspaceRead));
    }

    #[test]
    fn unavailable_default_names_the_public_release_image_and_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let error = install_defaults_with(&workspace, |_, _| Err("registry denied anonymous pull".into()))
            .expect_err("unavailable image");
        assert!(error.contains(DEFAULT_EXTENSIONS[0].1));
        assert!(error.contains(env!("CARGO_PKG_VERSION")));
        assert!(error.contains("check registry access"));
        assert!(error.contains("retry workspace provisioning"));
    }

    #[test]
    fn wrong_default_identity_is_actionable_and_a_corrected_retry_is_clean() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let manifest = |name: &str| Manifest {
            name: ExtensionName::new(name).unwrap(),
            display_name: name.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new([Capability::WorkspaceRead, Capability::Interface]),
            entrypoint: None,
            activation: Activation::Workspace,
            interface: Some(Presentation {
                tab_title: name.to_owned(),
                icon: None,
            }),
            pane_providers: Vec::new(),
            resources: Resources::default(),
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };

        let error = install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:spoof".to_owned(),
                manifest: manifest("storybook"),
            })
        })
        .expect_err("wrong default identity");

        assert!(error.contains(DEFAULT_EXTENSIONS[0].1));
        assert!(error.contains(env!("CARGO_PKG_VERSION")));
        assert!(error.contains("declares extension storybook, expected top"));
        assert!(error.contains("verify the image publisher and manifest"));
        assert!(error.contains("retry workspace provisioning"));
        assert!(Roster::workspace(&workspace).unwrap().entries().is_empty());

        install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:top".to_owned(),
                manifest: manifest("top"),
            })
        })
        .expect("corrected retry");

        let entries = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.as_str(), "top");
        assert_eq!(entries[0].image_digest, "sha256:top");
        assert_eq!(entries[0].stage, Stage::Duty);
    }

    #[test]
    fn retry_replaces_a_same_tag_default_with_the_current_image_digest() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let manifest = Manifest {
            name: ExtensionName::new("top").unwrap(),
            display_name: "Top".into(),
            version: "0.4.0".into(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new([Capability::WorkspaceRead, Capability::Interface]),
            entrypoint: None,
            activation: Activation::Workspace,
            interface: Some(Presentation {
                tab_title: "Top".into(),
                icon: None,
            }),
            pane_providers: Vec::new(),
            resources: Resources::default(),
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
        };
        // Model interruption after the record was saved but before provisioning
        // enabled it. The mutable release tag may resolve differently on retry.
        Roster::workspace(&workspace)
            .unwrap()
            .register(&manifest, "sha256:old-top", &manifest.capabilities, 1)
            .unwrap();
        assert_eq!(
            Roster::workspace(&workspace).unwrap().entries()[0].stage,
            Stage::Standby
        );
        let mut acquisitions = 0;
        install_defaults_with(&workspace, |_, reference| {
            acquisitions += 1;
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:current-top".to_owned(),
                manifest: manifest.clone(),
            })
        })
        .unwrap();

        assert_eq!(acquisitions, 1, "retry must inspect the current release image");
        let entries = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].image_digest, "sha256:current-top");
        assert_eq!(entries[0].version, "0.4.0");
        assert_eq!(entries[0].stage, Stage::Duty);
    }
}
