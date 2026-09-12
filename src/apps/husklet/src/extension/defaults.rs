//! First-party extensions installed while a workspace is provisioned.

use hl_extension::{Activation, ExtensionName, Manifest, Stage};

use super::{Candidate, Roster};
use crate::config::WorkspaceConfig;

/// Ordered identities and release-matched image references for a new workspace.
///
/// This order is also their default order in the workspace sidebar.
const TOP_IMAGE: &str = match option_env!("HL_TOP_IMAGE") {
    Some(reference) => reference,
    None => concat!("ghcr.io/husklet/husklet/extension-top:", env!("CARGO_PKG_VERSION")),
};

pub const DEFAULT_EXTENSIONS: [(&str, &str); 1] = [("top", TOP_IMAGE)];

/// Acquires, grants, records, and enables the trusted first-party control surface.
///
/// A retry inspects the release reference again before trusting retained state.
/// Tags are human release coordinates rather than immutable identity: a failed
/// provisioning attempt may have left a record from an earlier image carrying
/// the same tag, and the current digest must replace it before the workspace is
/// allowed to start.
pub fn install_defaults(workspace: &WorkspaceConfig) -> Result<(), String> {
    install_defaults_with(workspace, |workspace, reference| {
        if immutable_release_reference(reference) {
            Candidate::read(workspace, reference)
        } else {
            Candidate::read_fresh(workspace, reference)
        }
    })
}

fn immutable_release_reference(reference: &str) -> bool {
    reference.rsplit_once("@sha256:").is_some_and(|(_, digest)| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
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
        if !provides_default_surface(&candidate.manifest) {
            return Err(format!(
                "default extension {expected} {version} could not be installed from public image {reference}: its manifest must provide an automatically activated interface; verify the image publisher and manifest, then retry workspace provisioning",
                version = env!("CARGO_PKG_VERSION")
            ));
        }
        let trusted = trusted_manifest(expected)?;
        if candidate.manifest != trusted {
            return Err(format!(
                "default extension {expected} {version} could not be installed from public image {reference}: its manifest does not match the first-party release contract, so Husklet refused to grant it authority automatically; verify the published image, then retry workspace provisioning",
                version = env!("CARGO_PKG_VERSION")
            ));
        }
        if let Some(entry) = roster.entries().into_iter().find(|entry| entry.name == name) {
            if entry.image_digest != candidate.digest || !roster.matches_manifest(&name, &candidate.manifest) {
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

fn trusted_manifest(name: &str) -> Result<Manifest, String> {
    let source = match name {
        "top" => include_str!("../../../../../extensions/top/extension.toml"),
        _ => return Err(format!("default extension {name} has no first-party manifest contract")),
    };
    let mut manifest = Manifest::parse(source, hl_extension::PROTOCOL)
        .map_err(|error| format!("default extension {name} has an invalid first-party manifest contract: {error}"))?;
    // First-party Dockerfiles stamp the application release into the copied
    // manifest. Compare candidates to that shipped document, not the source
    // template's independent development version.
    manifest.version = env!("CARGO_PKG_VERSION").to_owned();
    Ok(manifest)
}

fn provides_default_surface(manifest: &Manifest) -> bool {
    manifest.interface.is_some() && matches!(manifest.activation, Activation::Workspace | Activation::Tab)
}

fn moment() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_extension::{Activation, Capability, Grant, Presentation};

    fn top_manifest() -> Manifest {
        trusted_manifest("top").expect("checked-in Top manifest")
    }

    #[test]
    fn defaults_are_release_matched_and_sidebar_ordered() {
        assert_eq!(DEFAULT_EXTENSIONS[0].0, "top");
        for (name, reference) in DEFAULT_EXTENSIONS {
            let fallback = format!("ghcr.io/husklet/husklet/extension-{name}:{}", env!("CARGO_PKG_VERSION"));
            let expected = option_env!("HL_TOP_IMAGE").unwrap_or(&fallback);
            assert_eq!(reference, expected);
            if option_env!("HL_TOP_IMAGE").is_some() {
                assert!(immutable_release_reference(reference));
            }
        }
    }

    #[test]
    fn only_a_complete_sha256_reference_is_immutable() {
        let digest = "a".repeat(64);
        assert!(immutable_release_reference(&format!(
            "registry.test/top:0.4.0@sha256:{digest}"
        )));
        assert!(!immutable_release_reference("registry.test/top:0.4.0"));
        assert!(!immutable_release_reference("registry.test/top:0.4.0@sha256:abc"));
        assert!(!immutable_release_reference(&format!(
            "registry.test/top:0.4.0@sha256:{}",
            "z".repeat(64)
        )));
    }

    #[test]
    fn trusted_top_manifest_matches_the_dockerfile_release_stamp() {
        let source = Manifest::parse(
            include_str!("../../../../../extensions/top/extension.toml"),
            hl_extension::PROTOCOL,
        )
        .expect("checked-in Top manifest template");
        let shipped = top_manifest();
        let dockerfile = include_str!("../../../../../extensions/top/Dockerfile");

        assert_ne!(
            source.version,
            env!("CARGO_PKG_VERSION"),
            "fixture must exercise stamping"
        );
        assert!(dockerfile.contains("sed -i \"s/^version = .*/version = \\\"${HUSKLET_EXTENSION_VERSION}\\\"/\""));
        assert_eq!(shipped.version, env!("CARGO_PKG_VERSION"));
        let mut expected = source;
        expected.version = env!("CARGO_PKG_VERSION").to_owned();
        assert_eq!(
            shipped, expected,
            "runtime must model only the Dockerfile's version stamp"
        );
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
            let manifest = top_manifest();
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: format!("sha256:{name}"),
                manifest,
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
        let manifest = |name: &str| {
            let mut manifest = top_manifest();
            manifest.name = ExtensionName::new(name).unwrap();
            manifest.display_name = name.to_owned();
            manifest
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
                manifest: top_manifest(),
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
    fn unusable_default_surface_is_rejected_and_a_corrected_retry_reaches_duty() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let candidate = |reference: &str, activation, interface| {
            let mut manifest = top_manifest();
            manifest.activation = activation;
            manifest.interface = interface;
            Candidate {
                reference: reference.to_owned(),
                digest: "sha256:top".to_owned(),
                manifest,
            }
        };

        let error = install_defaults_with(&workspace, |_, reference| {
            Ok(candidate(
                reference,
                Activation::Manual,
                Some(Presentation {
                    tab_title: "Top".into(),
                    icon: None,
                }),
            ))
        })
        .expect_err("manually activated default surface");

        assert!(error.contains(DEFAULT_EXTENSIONS[0].1));
        assert!(error.contains("must provide an automatically activated interface"));
        assert!(error.contains("verify the image publisher and manifest"));
        assert!(error.contains("retry workspace provisioning"));
        assert!(Roster::workspace(&workspace).unwrap().entries().is_empty());

        let error = install_defaults_with(&workspace, |_, reference| {
            Ok(candidate(reference, Activation::Workspace, None))
        })
        .expect_err("default without an interface");
        assert!(error.contains("must provide an automatically activated interface"));
        assert!(Roster::workspace(&workspace).unwrap().entries().is_empty());

        install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:top".to_owned(),
                manifest: top_manifest(),
            })
        })
        .expect("corrected retry");

        let entries = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].image_digest, "sha256:top");
        assert_eq!(entries[0].stage, Stage::Duty);
    }

    #[test]
    fn checked_in_top_manifest_provides_the_default_surface_contract() {
        let manifest = Manifest::parse(
            include_str!("../../../../../extensions/top/extension.toml"),
            hl_extension::PROTOCOL,
        )
        .expect("checked-in Top manifest");

        assert_eq!(manifest.activation, Activation::Tab);
        assert!(provides_default_surface(&manifest));
    }

    #[test]
    fn automatic_authority_is_bound_to_the_checked_in_manifest_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let trusted = top_manifest();
        Roster::workspace(&workspace)
            .unwrap()
            .register(&trusted, "sha256:usable-top", &trusted.capabilities, 1)
            .unwrap();
        let mut broadened = trusted.clone();
        broadened.capabilities = Grant::new(trusted.capabilities.iter().chain([Capability::FilesystemWrite]));

        let error = install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:untrusted-top".to_owned(),
                manifest: broadened.clone(),
            })
        })
        .expect_err("broadened automatic authority");

        assert!(error.contains("does not match the first-party release contract"));
        assert!(error.contains("refused to grant it authority automatically"));
        assert!(error.contains("verify the published image"));
        let retained = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].image_digest, "sha256:usable-top");
        assert_eq!(retained[0].stage, Stage::Standby);

        install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:corrected-top".to_owned(),
                manifest: trusted.clone(),
            })
        })
        .expect("corrected first-party image");
        let corrected = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(corrected.len(), 1);
        assert_eq!(corrected[0].image_digest, "sha256:corrected-top");
        assert_eq!(corrected[0].stage, Stage::Duty);
    }

    #[test]
    fn retry_replaces_a_same_tag_default_with_the_current_image_digest() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let manifest = top_manifest();
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
        assert_eq!(entries[0].version, manifest.version);
        assert_eq!(entries[0].stage, Stage::Duty);
    }

    #[test]
    fn same_digest_retry_repairs_retained_authority_before_enabling() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let trusted = top_manifest();
        let mut stale = trusted.clone();
        stale.capabilities = Grant::new(trusted.capabilities.iter().chain([Capability::FilesystemWrite]));
        Roster::workspace(&workspace)
            .unwrap()
            .register_resource_scoped(
                &stale,
                "sha256:same-top",
                &stale.capabilities,
                &stale.containers,
                &stale.images,
                &stale.networks,
                &stale.volumes,
                &stale.filesystem,
                &stale.workspace_environment,
                1,
            )
            .unwrap();

        install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:same-top".to_owned(),
                manifest: trusted.clone(),
            })
        })
        .expect("same-digest authority repair");

        let repaired = Roster::workspace(&workspace).unwrap().entries();
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].image_digest, "sha256:same-top");
        assert_eq!(repaired[0].granted, trusted.capabilities);
        assert!(!repaired[0].granted.holds(Capability::FilesystemWrite));
        assert!(Roster::workspace(&workspace)
            .unwrap()
            .matches_manifest(&trusted.name, &trusted));
        assert_eq!(repaired[0].stage, Stage::Duty);
    }

    #[test]
    fn same_digest_retry_repairs_retained_launch_contract_before_enabling() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = WorkspaceConfig::new("demo", "alpine:3.20", hl_ws::Arch::Amd64);
        workspace.storage = Some(directory.path().join("workspace"));
        let trusted = top_manifest();
        let mut stale = trusted.clone();
        stale.entrypoint = Some(vec!["/bin/false".to_owned()]);
        Roster::workspace(&workspace)
            .unwrap()
            .register_resource_scoped(
                &stale,
                "sha256:same-top",
                &stale.capabilities,
                &stale.containers,
                &stale.images,
                &stale.networks,
                &stale.volumes,
                &stale.filesystem,
                &stale.workspace_environment,
                1,
            )
            .unwrap();

        install_defaults_with(&workspace, |_, reference| {
            Ok(Candidate {
                reference: reference.to_owned(),
                digest: "sha256:same-top".to_owned(),
                manifest: trusted.clone(),
            })
        })
        .expect("same-digest launch contract repair");

        let repaired = Roster::workspace(&workspace).unwrap();
        assert!(repaired.matches_manifest(&trusted.name, &trusted));
        assert_eq!(repaired.stage(&trusted.name), Stage::Duty);
    }
}
