//! Docker wire models owned by `hl-daemon` and reused verbatim by the client.

pub use hl_daemon::api::{
    Attachment, Authentication, BindOptions, BindReadOnly, BuildPrune, Change, ChangeKind, CommitOptions, ConfigFrom,
    Container, ContainerConfig, ContainerCreation, ContainerDetails, ContainerLogs, ContainerPrune, ContainerState,
    Cpu, CpuUsage, CreateContainer, Credentials, DiskUsage, Distribution, DockerMount, DriverConfig, EndpointConfig,
    EndpointIpam, EndpointsConfig, Event, EventFilter, EventQuery, ExecAttach, ExecConfig, ExecCreated, ExecInspect,
    ExecOpen, ExecProcess, ExecStart, ExposedPorts, HealthLog, HealthState, Healthcheck, HostConfig, ImageCommit,
    ImageConfig, ImageDelete, ImageHistory, ImageLoad, ImagePrune, ImageSummary, InspectContainer, InspectImage, Ipam,
    IpamConfig, List, LogOptions, LogStreams, Memory, MountPoint, Network, NetworkConnect, NetworkContainer,
    NetworkCreate, NetworkCreated, NetworkDisconnect, NetworkPrune, NetworkSettings, NetworkingConfig, PathStat, Pids,
    Plugin, PortBinding, PortBindings, PortSummary, ProgressDetail, PullProgress, PushAux, PushProgress, RestartPolicy,
    Search, Stats, SystemInfo, SystemPrune, Throttling, Top, Update, UpdateResult, UsageData, Version, Volume,
    VolumeCreate, VolumeList, VolumeOptions, VolumePrune, VolumeUsage, Wait,
};

#[cfg(test)]
mod tests {
    use super::{ProgressDetail, PullProgress};

    #[test]
    fn image_progress_reuses_the_daemon_owned_wire_model() {
        let progress: PullProgress = serde_json::from_value(serde_json::json!({
            "status": "Downloading",
            "id": "layer",
            "progressDetail": {"current": 7, "total": 11}
        }))
        .unwrap();
        assert_eq!(progress.progress_detail, Some(ProgressDetail { current: 7, total: 11 }));
    }
}
