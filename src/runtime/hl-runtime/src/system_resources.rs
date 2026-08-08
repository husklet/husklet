/// Container-visible resource values shared by syscalls and virtual files.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct ResourceSnapshot {
    pub uptime_seconds: u64,
    /// Successful process creations since this runtime instance started.
    pub process_creations: u64,
    pub loads: [u64; 3],
    pub total_memory: u64,
    pub free_memory: u64,
    /// Explicit CPU quota; `None` keeps `cpu.max` unlimited even when topology is finite.
    pub cpu_limit: Option<usize>,
    /// Explicit process quota; `None` keeps `pids.max` unlimited even though registry capacity is finite.
    pub process_limit: Option<usize>,
}

/// One coherent observation of the guest-visible boot and resource tuple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemView {
    pub boot: [u8; 16],
    pub resources: ResourceSnapshot,
}

impl ResourceSnapshot {
    /// Container-visible memory when no host or cgroup observation exists.
    ///
    /// The fallback matches the retained engine and remains distinct from the
    /// zero sentinel used to render an unlimited cgroup.
    #[must_use]
    pub fn visible_memory(self) -> (u64, u64) {
        const FALLBACK_TOTAL: u64 = 8_u64 << 30;
        if self.total_memory == 0 {
            (FALLBACK_TOTAL, FALLBACK_TOTAL / 4)
        } else {
            (self.total_memory, self.free_memory.min(self.total_memory))
        }
    }
}
