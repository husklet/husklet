//! `/containers/{id}/stats` document DTOs.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ---- containers: `docker stats` --------------------------------------------
// The stats document keeps Docker's LOWERCASE snake_case keys verbatim (NOT PascalCase), so these
// structs use their plain field names with no rename.

#[derive(Serialize)]
pub(crate) struct ContainerStats {
    pub read: String,
    /// Timestamp of the `precpu_stats` sample. Docker's Go zero-time (`0001-01-01T00:00:00Z`) on a
    /// one-shot / first sample; present so strict clients (bollard's non-Option `preread`) deserialize.
    pub preread: String,
    pub name: String,
    pub id: String,
    pub pids_stats: PidsStats,
    pub cpu_stats: CpuStats,
    pub precpu_stats: CpuStats,
    pub memory_stats: MemoryStats,
    pub blkio_stats: BlkioStats,
    /// Always the empty object `{}` (hl reports no per-interface net counters).
    pub networks: BTreeMap<String, Value>,
    /// Process count (Docker's Windows-oriented field; 0 on Linux). Present for strict-client parsing.
    pub num_procs: u32,
    /// Always the empty object `{}` (hl has no blkio/storage backend accounting).
    pub storage_stats: BTreeMap<String, Value>,
}

#[derive(Serialize)]
pub(crate) struct PidsStats {
    pub current: u64,
}

/// One `cpu_stats`/`precpu_stats` block in Docker's shape.
#[derive(Serialize)]
pub(crate) struct CpuStats {
    pub cpu_usage: CpuUsage,
    pub system_cpu_usage: u64,
    pub online_cpus: u32,
    pub throttling_data: ThrottlingData,
}

impl CpuStats {
    /// Build one `cpu_stats`/`precpu_stats` block from the sampled process and host totals.
    pub(crate) fn new(total: u64, system: u64) -> Self {
        Self {
            cpu_usage: CpuUsage {
                total_usage: total,
                usage_in_kernelmode: 0,
                usage_in_usermode: total,
            },
            system_cpu_usage: system,
            online_cpus: 1,
            throttling_data: ThrottlingData {
                periods: 0,
                throttled_periods: 0,
                throttled_time: 0,
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct CpuUsage {
    pub total_usage: u64,
    pub usage_in_kernelmode: u64,
    pub usage_in_usermode: u64,
}

#[derive(Serialize)]
pub(crate) struct ThrottlingData {
    pub periods: u64,
    pub throttled_periods: u64,
    pub throttled_time: u64,
}

#[derive(Serialize)]
pub(crate) struct MemoryStats {
    pub usage: u64,
    /// Peak usage — docker-compat clients read `max_usage`; hl has no historical high-water mark, so it
    /// mirrors `usage` (a consistent value beats an omitted key strict clients choke on).
    pub max_usage: u64,
    pub limit: u64,
    /// Memory-limit-hit counter — always 0 (hl doesn't OOM-throttle), but present for client compat.
    pub failcnt: u64,
    /// Always the empty object `{}` (hl has no cgroup memory breakdown).
    pub stats: BTreeMap<String, Value>,
}

/// All eight recursive blkio arrays, always empty (hl has no block-IO accounting).
#[derive(Serialize)]
pub(crate) struct BlkioStats {
    pub io_service_bytes_recursive: Vec<Value>,
    pub io_serviced_recursive: Vec<Value>,
    pub io_queue_recursive: Vec<Value>,
    pub io_service_time_recursive: Vec<Value>,
    pub io_wait_time_recursive: Vec<Value>,
    pub io_merged_recursive: Vec<Value>,
    pub io_time_recursive: Vec<Value>,
    pub sectors_recursive: Vec<Value>,
}

impl BlkioStats {
    pub fn empty() -> Self {
        BlkioStats {
            io_service_bytes_recursive: vec![],
            io_serviced_recursive: vec![],
            io_queue_recursive: vec![],
            io_service_time_recursive: vec![],
            io_wait_time_recursive: vec![],
            io_merged_recursive: vec![],
            io_time_recursive: vec![],
            sectors_recursive: vec![],
        }
    }
}
