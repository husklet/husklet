use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker-compatible process listing for a running container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Top {
    pub titles: Vec<String>,
    pub processes: Vec<Vec<String>>,
}

/// Docker-compatible resource sample.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Stats {
    pub read: String,
    pub preread: String,
    pub name: String,
    pub id: String,
    pub pids_stats: Pids,
    pub cpu_stats: Cpu,
    pub precpu_stats: Cpu,
    pub memory_stats: Memory,
    pub blkio_stats: BlockIo,
    pub networks: BTreeMap<String, serde_json::Value>,
    pub num_procs: u32,
    pub storage_stats: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Pids {
    pub current: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cpu {
    pub cpu_usage: CpuUsage,
    pub system_cpu_usage: u64,
    pub online_cpus: u32,
    pub throttling_data: Throttling,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CpuUsage {
    pub total_usage: u64,
    pub usage_in_kernelmode: u64,
    pub usage_in_usermode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Throttling {
    pub periods: u64,
    pub throttled_periods: u64,
    pub throttled_time: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Memory {
    pub usage: u64,
    pub max_usage: u64,
    pub limit: u64,
    pub failcnt: u64,
    pub stats: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockIo {
    pub io_service_bytes_recursive: Vec<serde_json::Value>,
    pub io_serviced_recursive: Vec<serde_json::Value>,
    pub io_queue_recursive: Vec<serde_json::Value>,
    pub io_service_time_recursive: Vec<serde_json::Value>,
    pub io_wait_time_recursive: Vec<serde_json::Value>,
    pub io_merged_recursive: Vec<serde_json::Value>,
    pub io_time_recursive: Vec<serde_json::Value>,
    pub sectors_recursive: Vec<serde_json::Value>,
}

impl BlockIo {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            io_service_bytes_recursive: Vec::new(),
            io_serviced_recursive: Vec::new(),
            io_queue_recursive: Vec::new(),
            io_service_time_recursive: Vec::new(),
            io_wait_time_recursive: Vec::new(),
            io_merged_recursive: Vec::new(),
            io_time_recursive: Vec::new(),
            sectors_recursive: Vec::new(),
        }
    }
}
