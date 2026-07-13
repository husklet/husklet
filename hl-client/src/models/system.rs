/// Engine info for the System view (`/version` + `/info`, flattened).
#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    /// Engine version reported by `/version`.
    pub version: String,
    /// Docker API version the engine speaks.
    pub api_version: String,
    /// Operating system the engine runs on.
    pub os: String,
    /// CPU architecture the engine runs on.
    pub arch: String,
    /// Kernel version of the engine host.
    pub kernel: String,
    /// Storage driver in use (e.g. `overlay2`).
    pub driver: String,
    /// Root directory where the engine stores its data.
    pub root_dir: String,
    /// Engine version reported by `/info` (`ServerVersion`).
    pub server_version: String,
    /// Number of CPUs available to the engine.
    pub ncpu: i64,
    /// Total memory available to the engine, in bytes.
    pub mem_total: i64,
    /// Total number of containers (any state).
    pub containers: i64,
    /// Number of running containers.
    pub running: i64,
    /// Number of paused containers.
    pub paused: i64,
    /// Number of stopped containers.
    pub stopped: i64,
    /// Number of images stored locally.
    pub images: i64,
}

/// Disk usage (`/system/df`).
#[derive(Debug, Clone, Default)]
pub struct DiskUsage {
    /// Total size of all image layers, in bytes.
    pub layers_size: i64,
    /// Number of images.
    pub images: i64,
    /// Number of containers.
    pub containers: i64,
    /// Number of volumes.
    pub volumes: i64,
}
