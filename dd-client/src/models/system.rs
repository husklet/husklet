/// Engine info for the System view (`/version` + `/info`, flattened).
#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    pub version: String,
    pub api_version: String,
    pub os: String,
    pub arch: String,
    pub kernel: String,
    pub driver: String,
    pub root_dir: String,
    pub server_version: String,
    pub ncpu: i64,
    pub mem_total: i64,
    pub containers: i64,
    pub running: i64,
    pub paused: i64,
    pub stopped: i64,
    pub images: i64,
}

/// Disk usage (`/system/df`).
#[derive(Debug, Clone, Default)]
pub struct DiskUsage {
    pub layers_size: i64,
    pub images: i64,
    pub containers: i64,
    pub volumes: i64,
}
