//! Local, registry-free workspace seed shared by checkpoint acceptance journeys.

use super::{Configuration, CONTAINER};
use crate::config::{WorkspaceConfig, WorkspaceStore};
use hl_container::{Config, ContainerSpec, Guest, Isolation, Process, Sandbox};
use std::path::{Path, PathBuf};

pub type SeedResult<T> = Result<T, Box<dyn std::error::Error>>;

pub struct SeededWorkspace {
    pub workspace: WorkspaceConfig,
    pub rootfs: PathBuf,
    pub container_state: PathBuf,
    pub container_journal: PathBuf,
}

pub struct SeededContainer {
    pub name: String,
    pub rootfs: PathBuf,
    pub state: PathBuf,
}

/// Publishes one workspace and its primary container from an already unpacked rootfs.
pub async fn seed(
    home: &Path,
    storage: &Path,
    rootfs: &Path,
    arch: hl_ws::Arch,
    script: &str,
) -> SeedResult<SeededWorkspace> {
    std::fs::create_dir_all(home)?;
    std::fs::create_dir_all(storage)?;
    let guest = match arch {
        hl_ws::Arch::Amd64 => Guest::X86_64,
        hl_ws::Arch::Arm64 => Guest::Aarch64,
    };
    let mut workspace = WorkspaceConfig::new(
        format!("continue-product-{}", std::process::id()),
        "fixture:local",
        arch,
    );
    workspace.storage = Some(storage.to_owned());
    workspace.docker_sock = false;
    WorkspaceStore::load(home.join(".hl/workspaces.conf"))?.upsert(workspace.clone())?;

    let checkpoints = std::sync::Arc::new(crate::runtime::checkpoint::WorkspaceCheckpoints::open(storage)?);
    let container_storage = storage.join("containers");
    let containers = hl_container::Containers::builder(Config::new(container_storage.clone()))
        .checkpoints(checkpoints)
        .build()
        .await?;
    let configuration = Configuration::new(&workspace);
    let signature = configuration.signature()?;
    let configuration_signature = configuration.identity_signature()?;
    let runtime_signature = configuration.runtime_signature();
    let session = crate::runtime::session::Session::from_root("", rootfs)?;
    let spec = ContainerSpec::from_directory(rootfs, Process::new("/bin/sh").args(["-c", script]))
        .name(CONTAINER)
        .guest(guest)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            read_only_root: false,
            network_isolated: true,
            seccomp_baseline: hl_container::SeccompBaseline::Container,
        });
    let spec = session.label(configuration.container(spec, signature, configuration_signature, runtime_signature));
    let seeded = containers.create(spec).await?;
    crate::runtime::session::Session::from_labels(&seeded.spec.labels)?;
    session.provision(&containers).await?;
    let state = container_storage.join("state/containers");
    Ok(SeededWorkspace {
        workspace,
        rootfs: rootfs.to_owned(),
        container_state: state.join(format!("{}.json", seeded.id)),
        container_journal: state.join(format!("{}.journal", seeded.id)),
    })
}

/// Adds a distinct created container to a seeded workspace.
pub async fn seed_additional(
    storage: &Path,
    rootfs: &Path,
    workspace: &WorkspaceConfig,
    name: &str,
    script: &str,
) -> SeedResult<SeededContainer> {
    let checkpoints = std::sync::Arc::new(crate::runtime::checkpoint::WorkspaceCheckpoints::open(storage)?);
    let container_storage = storage.join("containers");
    let containers = hl_container::Containers::builder(Config::new(container_storage.clone()))
        .checkpoints(checkpoints)
        .build()
        .await?;
    let configuration = Configuration::new(workspace);
    let signature = configuration.signature()?;
    let configuration_signature = configuration.identity_signature()?;
    let runtime_signature = configuration.runtime_signature();
    let session = crate::runtime::session::Session::from_root("", rootfs)?;
    let guest = match workspace.arch {
        hl_ws::Arch::Amd64 => Guest::X86_64,
        hl_ws::Arch::Arm64 => Guest::Aarch64,
    };
    let spec = ContainerSpec::from_directory(rootfs, Process::new("/bin/sh").args(["-c", script]))
        .guest(guest)
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            read_only_root: false,
            network_isolated: true,
            seccomp_baseline: hl_container::SeccompBaseline::Container,
        });
    let spec = session
        .label(configuration.container(spec, signature, configuration_signature, runtime_signature))
        .name(name);
    let seeded = containers.create(spec).await?;
    session.provision(&containers).await?;
    let control = storage.join("state/gui-checkpoint-secondary");
    if let Some(parent) = control.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(control, format!("{name}\n"))?;
    let state = container_storage.join("state/containers");
    Ok(SeededContainer {
        name: name.to_owned(),
        rootfs: rootfs.to_owned(),
        state: state.join(format!("{}.json", seeded.id)),
    })
}
