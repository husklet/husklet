#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;

pub(super) struct MemoryAccount {
    limit: u64,
    current: Mutex<u64>,
    system: hl_runtime::SystemObservationHandle,
}

impl std::fmt::Debug for MemoryAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryAccount")
            .field("limit", &self.limit)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl MemoryAccount {
    pub(super) fn new(limit: u64, system: hl_runtime::SystemObservationHandle) -> Self {
        Self {
            limit,
            current: Mutex::new(0),
            system,
        }
    }

    fn publish(&self, current: u64) -> bool {
        self.system
            .observe_free_memory(self.limit.saturating_sub(current))
            .is_ok()
    }
}

impl hl_runtime::AnonymousMemoryAccount for MemoryAccount {
    fn reserve(&self, bytes: u64) -> bool {
        let mut current = self.current.lock().unwrap_or_else(|error| error.into_inner());
        let Some(next) = current.checked_add(bytes).filter(|next| *next <= self.limit) else {
            return false;
        };
        if !self.publish(next) {
            return false;
        }
        *current = next;
        true
    }

    fn refund(&self, bytes: u64) {
        let mut current = self.current.lock().unwrap_or_else(|error| error.into_inner());
        *current = current.saturating_sub(bytes);
        let _ = self.publish(*current);
    }

    fn current(&self) -> u64 {
        *self.current.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_runtime::AnonymousMemoryAccount;

    #[test]
    fn current_updates_visible_memory() {
        let system = Arc::new(
            hl_runtime::SystemAuthority::new(hl_runtime::ResourceSnapshot {
                total_memory: 100,
                free_memory: 100,
                ..hl_runtime::ResourceSnapshot::default()
            })
            .unwrap(),
        );
        let mut launch = system.prepare_launch(b"memory-account", system.snapshot()).unwrap();
        let observer = launch.construction_observer();
        launch.commit();
        let account = MemoryAccount::new(100, observer);
        system.observe_uptime(17);
        system.observe_fork();
        assert!(account.reserve(37));
        assert_eq!(account.current(), 37);
        assert_eq!(system.snapshot().free_memory, 63);
        assert_eq!(system.snapshot().uptime_seconds, 17);
        assert_eq!(system.snapshot().process_creations, 1);
        assert!(!account.reserve(64));
        assert_eq!(account.current(), 37);
        account.refund(12);
        assert_eq!(account.current(), 25);
        assert_eq!(system.snapshot().free_memory, 75);
    }
}
