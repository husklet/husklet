use serde::{Deserialize, Serialize};

mod auth;
mod configuration;
mod container;
mod create;
mod exec;
mod filesystem;
#[cfg(feature = "runtime")]
mod format;
mod image;
mod inspect;
#[cfg(feature = "runtime")]
mod lifecycle;
mod log;
mod process;
mod stats;
mod system;
#[cfg(feature = "runtime")]
mod timestamp;

pub use auth::{Authentication, Credentials};
pub use configuration::{CompatibilityFields, Healthcheck, RestartPolicy, Update, UpdateResult};
pub use container::{Container, ContainerDetails, ContainerMetadata, ContainerPrune, Wait};
pub use create::{
    BindOptions, BindReadOnly, ContainerCreation, CreateContainer, DockerMount, DriverConfig, EndpointsConfig,
    HostConfig, NetworkingConfig, VolumeOptions,
};
#[cfg(feature = "runtime")]
pub(crate) use exec::console_size;
pub use exec::{
    Attachment, Console, ExecAttach, ExecCatalogue, ExecConfig, ExecCreated, ExecInspect, ExecLifetime, ExecNetwork, ExecOpen, ExecOutput,
    ExecProcess, ExecStart,
};
pub use filesystem::{Change, ChangeKind, PathStat};
pub use image::{
    BuildPrune, CommitOptions, Distribution, ImageCommit, ImageConfig, ImageDelete, ImageHistory, ImageLoad,
    ImagePrune, ImageSummary, InspectImage, ProgressDetail, PullProgress, PushAux, PushProgress, Search,
};
pub use inspect::{
    ContainerConfig, ContainerState, EndpointSettings, HealthLog, HealthState, HostInspection, InspectContainer,
    MountPoint, NetworkSettings,
};
#[cfg(feature = "runtime")]
pub(crate) use log::LogEncoder;
pub use log::{ContainerLogs, LogOptions, LogProtocolError, LogStreams};
pub use process::{EnvError, EnvVar, EnvVars};
pub use stats::{BlockIo, Cpu, CpuUsage, Memory, Pids, Stats, Throttling, Top};
pub use system::{DiskUsage, Plugin, SystemInfo, SystemPrune, UsageData, Version, VolumeUsage};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DockerError {
    pub message: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn container_admin_models_use_exact_docker_wire_shapes() {
        assert_eq!(
            serde_json::to_value(super::Change {
                path: "/tmp/new".into(),
                kind: super::ChangeKind::Added,
            })
            .unwrap(),
            serde_json::json!({"Path":"/tmp/new","Kind":1})
        );
        assert_eq!(
            serde_json::to_value(super::UpdateResult::default()).unwrap(),
            serde_json::json!({"Warnings":[]})
        );
        assert_eq!(
            serde_json::to_value(super::ContainerPrune {
                containers_deleted: vec!["container-id".into()],
                space_reclaimed: 42,
            })
            .unwrap(),
            serde_json::json!({
                "ContainersDeleted":["container-id"],
                "SpaceReclaimed":42
            })
        );
    }
}
