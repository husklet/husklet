/// Linux task scheduling state retained independently of the host scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulingProfile {
    policy: u32,
    priority: i32,
    nice: i8,
    reset_on_fork: bool,
}

impl SchedulingProfile {
    pub const OTHER: Self = Self {
        policy: 0,
        priority: 0,
        nice: 0,
        reset_on_fork: false,
    };

    #[must_use]
    pub const fn non_realtime(policy: u32, reset_on_fork: bool) -> Option<Self> {
        match policy {
            0 | 3 | 5 | 6 => Some(Self {
                policy,
                priority: 0,
                nice: 0,
                reset_on_fork,
            }),
            _ => None,
        }
    }

    #[must_use]
    pub const fn restore(policy: u32, priority: i32, reset_on_fork: bool) -> Option<Self> {
        let Some(profile) = Self::non_realtime(policy, reset_on_fork) else {
            return None;
        };
        profile.with_priority(priority)
    }

    #[must_use]
    pub const fn policy(self) -> u32 {
        self.policy
    }
    #[must_use]
    pub const fn priority(self) -> i32 {
        self.priority
    }
    #[must_use]
    pub const fn nice(self) -> i8 {
        self.nice
    }
    #[must_use]
    pub const fn resets_on_fork(self) -> bool {
        self.reset_on_fork
    }

    #[must_use]
    pub const fn with_priority(self, priority: i32) -> Option<Self> {
        if priority == 0 {
            Some(Self { priority, ..self })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn with_nice(self, nice: i32) -> Self {
        Self {
            nice: if nice < -20 {
                -20
            } else if nice > 19 {
                19
            } else {
                nice as i8
            },
            ..self
        }
    }

    pub(crate) const fn fork_copy(self) -> Self {
        if self.reset_on_fork {
            Self::OTHER.with_nice(if self.nice < 0 { 0 } else { self.nice as i32 })
        } else {
            self
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{ProcessCredentials, ProcessLimits, RegistryConfig, SchedulingProfile, TaskRegistry};

    #[test]
    fn lifecycle_preserves_schedule() {
        let registry = TaskRegistry::new(RegistryConfig {
            online_cpus: 8,
            ..RegistryConfig::default()
        })
        .unwrap();
        let credentials = ProcessCredentials::new(0, 0, &[], 32).unwrap();
        let (_, leader) = registry.create_init(credentials, ProcessLimits::default()).unwrap();
        let batch = SchedulingProfile::non_realtime(3, false).unwrap().with_nice(5);
        registry.set_schedule(leader, batch).unwrap();
        let clone = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();
        assert_eq!(registry.schedule(clone).unwrap(), batch);
        let fork = registry.begin_fork_process(leader).unwrap();
        let (_, child) = registry.commit_fork_process(fork).unwrap();
        assert_eq!(registry.schedule(child).unwrap(), batch);

        let reset = SchedulingProfile::non_realtime(5, true).unwrap().with_nice(5);
        registry.set_schedule(leader, reset).unwrap();
        let fork = registry.begin_fork_process(leader).unwrap();
        let (_, child) = registry.commit_fork_process(fork).unwrap();
        assert_eq!(registry.schedule(child).unwrap(), SchedulingProfile::OTHER.with_nice(5));
        assert_eq!(registry.schedule(leader).unwrap(), reset);

        let reset = reset.with_nice(-5);
        registry.set_schedule(leader, reset).unwrap();
        let fork = registry.begin_fork_process(leader).unwrap();
        let (_, child) = registry.commit_fork_process(fork).unwrap();
        assert_eq!(registry.schedule(child).unwrap(), SchedulingProfile::OTHER);
    }
}
