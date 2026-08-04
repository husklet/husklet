const WORDS: usize = 16;

/// Immutable logical CPU topology advertised by one engine instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuTopology {
    online: u8,
}

impl CpuTopology {
    pub const MAXIMUM: usize = 64;

    pub fn new(online: usize) -> Result<Self, crate::TaskError> {
        let online = u8::try_from(online).map_err(|_| crate::TaskError::InvalidCapacity)?;
        if online == 0 || usize::from(online) > Self::MAXIMUM {
            return Err(crate::TaskError::InvalidCapacity);
        }
        Ok(Self { online })
    }

    #[must_use]
    pub const fn online(self) -> usize {
        self.online as usize
    }

    #[must_use]
    pub fn affinity(self) -> CpuAffinity {
        CpuAffinity::online(self.online())
    }

    #[must_use]
    pub fn range(self) -> String {
        if self.online == 1 {
            String::from("0")
        } else {
            format!("0-{}", self.online - 1)
        }
    }
}

/// Nonempty set of logical CPUs allowed for one guest thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuAffinity([u64; WORDS]);

impl CpuAffinity {
    pub const MAX_CPUS: usize = WORDS * 64;

    #[must_use]
    pub fn online(count: usize) -> Self {
        let count = count.clamp(1, Self::MAX_CPUS);
        let mut words = [0_u64; WORDS];
        for cpu in 0..count {
            words[cpu / 64] |= 1_u64 << (cpu % 64);
        }
        Self(words)
    }

    pub fn intersect(words: [u64; WORDS], online: Self) -> Option<Self> {
        let mut intersection = [0_u64; WORDS];
        for (index, word) in intersection.iter_mut().enumerate() {
            *word = words[index] & online.0[index];
        }
        intersection.iter().any(|word| *word != 0).then_some(Self(intersection))
    }

    pub fn from_words(words: [u64; WORDS]) -> Option<Self> {
        words.iter().any(|word| *word != 0).then_some(Self(words))
    }

    #[must_use]
    pub fn contains(self, cpu: usize) -> bool {
        cpu < Self::MAX_CPUS && self.0[cpu / 64] & (1_u64 << (cpu % 64)) != 0
    }

    #[must_use]
    pub fn mask_text(self) -> String {
        let mut groups = [0_u32; WORDS * 2];
        for (index, word) in self.0.iter().enumerate() {
            groups[index * 2] = *word as u32;
            groups[index * 2 + 1] = (*word >> 32) as u32;
        }
        let highest = groups.iter().rposition(|word| *word != 0).unwrap_or(0);
        let mut output = format!("{:x}", groups[highest]);
        for word in groups[..highest].iter().rev() {
            output.push_str(&format!(",{word:08x}"));
        }
        output
    }

    #[must_use]
    pub fn is_subset(self, other: Self) -> bool {
        self.0
            .iter()
            .zip(other.0)
            .all(|(value, allowed)| *value & !allowed == 0)
    }

    #[must_use]
    pub fn list_text(self) -> String {
        let mut ranges = Vec::new();
        let mut cpu = 0;
        while cpu < Self::MAX_CPUS {
            if !self.contains(cpu) {
                cpu += 1;
                continue;
            }
            let start = cpu;
            while cpu + 1 < Self::MAX_CPUS && self.contains(cpu + 1) {
                cpu += 1;
            }
            if start == cpu {
                ranges.push(start.to_string());
            } else {
                ranges.push(format!("{start}-{cpu}"));
            }
            cpu += 1;
        }
        ranges.join(",")
    }

    #[must_use]
    pub fn first(self) -> usize {
        self.0
            .iter()
            .enumerate()
            .find_map(|(index, word)| {
                (*word != 0).then_some(index * 64 + usize::try_from(word.trailing_zeros()).unwrap_or(0))
            })
            .unwrap_or(0)
    }

    #[must_use]
    pub const fn words(self) -> [u64; WORDS] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use crate::{CpuAffinity, ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
    use std::sync::Arc;

    #[test]
    fn lifecycle_preserves_affinity() {
        let registry = Arc::new(
            TaskRegistry::new(RegistryConfig {
                online_cpus: 8,
                ..RegistryConfig::default()
            })
            .unwrap(),
        );
        let credentials = ProcessCredentials::new(0, 0, &[], 32).unwrap();
        let (process, leader) = registry.create_init(credentials, ProcessLimits::default()).unwrap();
        let pinned =
            CpuAffinity::intersect([4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], CpuAffinity::online(8)).unwrap();
        registry.set_affinity(leader, pinned).unwrap();

        let clone = registry.begin_clone_thread(leader).unwrap();
        let worker = registry.commit_clone_thread(clone).unwrap();
        assert_eq!(registry.affinity(worker).unwrap(), pinned);

        let fork = registry.begin_fork_process(worker).unwrap();
        let (_, child) = registry.commit_fork_process(fork).unwrap();
        assert_eq!(registry.affinity(child).unwrap(), pinned);

        let snapshot = registry.snapshot();
        let restored = TaskRegistry::restore(&snapshot).unwrap();
        assert_eq!(restored.affinity(worker).unwrap(), pinned);
        assert_eq!(restored.affinity(child).unwrap(), pinned);
        assert_eq!(
            restored.affinity_target(leader, process.number() as i32).unwrap(),
            leader
        );
        assert_eq!(
            restored.affinity_target(leader, worker.number() as i32).unwrap(),
            worker
        );

        let worker_mask =
            CpuAffinity::intersect([8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], CpuAffinity::online(8)).unwrap();
        registry.set_affinity(worker, worker_mask).unwrap();
        let mut exec = registry.prepare_exec(process, worker).unwrap();
        exec.publish().unwrap();
        exec.finish();
        assert_eq!(registry.affinity(leader).unwrap(), worker_mask);
    }
}
