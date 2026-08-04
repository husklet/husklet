use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Container-visible resource values shared by syscalls and virtual files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceSnapshot {
    pub uptime_seconds: u64,
    /// Successful process creations since this runtime instance started.
    pub process_creations: u64,
    pub loads: [u64; 3],
    pub total_memory: u64,
    pub free_memory: u64,
    /// Explicit CPU quota; `None` keeps `cpu.max` unlimited even when topology is finite.
    pub cpu_limit: Option<usize>,
}

impl Default for ResourceSnapshot {
    fn default() -> Self {
        Self {
            uptime_seconds: 0,
            process_creations: 0,
            loads: [0; 3],
            total_memory: 0,
            free_memory: 0,
            cpu_limit: None,
        }
    }
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

/// Instance-scoped authority for resource projections.
pub struct SystemAuthority {
    resources: RwLock<ResourceSnapshot>,
    boot: RwLock<[u8; 16]>,
    sequence: AtomicU64,
}

impl SystemAuthority {
    pub fn new(snapshot: ResourceSnapshot) -> Self {
        Self {
            resources: RwLock::new(snapshot),
            boot: RwLock::new(Self::identity(b"hl-engine")),
            sequence: AtomicU64::new(1),
        }
    }

    pub fn snapshot(&self) -> ResourceSnapshot {
        *self.resources.read().unwrap_or_else(|error| error.into_inner())
    }

    pub fn replace(&self, snapshot: ResourceSnapshot) {
        *self.resources.write().unwrap_or_else(|error| error.into_inner()) = snapshot;
    }

    pub fn observe_uptime(&self, seconds: u64) {
        self.resources.write().unwrap_or_else(|error| error.into_inner()).uptime_seconds = seconds;
    }

    pub fn observe_fork(&self) {
        let mut snapshot = self.resources.write().unwrap_or_else(|error| error.into_inner());
        snapshot.process_creations = snapshot.process_creations.saturating_add(1);
    }

    #[must_use]
    pub fn boot_identity(&self) -> [u8; 16] {
        *self.boot.read().unwrap_or_else(|error| error.into_inner())
    }

    pub fn set_boot_key(&self, key: &[u8]) {
        *self.boot.write().unwrap_or_else(|error| error.into_inner()) = Self::identity(key);
        self.sequence.store(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn random_identity(&self) -> [u8; 16] {
        let serial = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut identity = [0; 16];
        for (index, chunk) in identity.chunks_exact_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (self.boot_identity(), serial, index).hash(&mut hasher);
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
        identity
    }

    fn identity(key: &[u8]) -> [u8; 16] {
        let mut identity = [0; 16];
        for (index, chunk) in identity.chunks_exact_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (key, index).hash(&mut hasher);
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
        identity
    }
}

impl Default for SystemAuthority {
    fn default() -> Self {
        Self::new(ResourceSnapshot::default())
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourceSnapshot, SystemAuthority};

    #[test]
    fn visible_memory_distinguishes_absent_observation_from_limit() {
        assert_eq!(ResourceSnapshot::default().visible_memory(), (8_u64 << 30, 2_u64 << 30));
        assert_eq!(
            ResourceSnapshot {
                total_memory: 4096,
                free_memory: 8192,
                ..ResourceSnapshot::default()
            }
            .visible_memory(),
            (4096, 4096),
        );
    }

    #[test]
    fn successful_forks_accumulate() {
        let system = SystemAuthority::default();
        system.observe_fork();
        system.observe_fork();
        assert_eq!(system.snapshot().process_creations, 2);
    }

    #[test]
    fn boot_identity_is_stable() {
        let first = SystemAuthority::default();
        let second = SystemAuthority::default();
        first.set_boot_key(b"container-a");
        second.set_boot_key(b"container-a");
        assert_eq!(first.boot_identity(), first.boot_identity());
        assert_eq!(first.boot_identity(), second.boot_identity());
        second.set_boot_key(b"container-b");
        assert_ne!(first.boot_identity(), second.boot_identity());
    }

    #[test]
    fn random_identity_is_fresh() {
        let system = SystemAuthority::default();
        assert_ne!(system.random_identity(), system.random_identity());
    }
}
