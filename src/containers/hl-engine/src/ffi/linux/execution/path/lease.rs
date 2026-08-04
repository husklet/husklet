use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_runtime::{FilesystemStats, GuestPath};

use super::NativeFile;

#[derive(Clone, Debug)]
pub(super) struct LeaseEntry {
    pub(super) guest: GuestPath,
    pub(super) filesystem: FilesystemStats,
    pub(super) file: std::sync::Weak<NativeFile>,
}

pub(super) type Registry = Arc<Mutex<BTreeMap<(u64, u64), usize>>>;

pub(super) struct WriteLease {
    identity: (u64, u64),
    writes: Registry,
}

impl WriteLease {
    pub(super) fn acquire(identity: (u64, u64), writes: Registry) -> Self {
        *writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(identity)
            .or_insert(0) += 1;
        Self { identity, writes }
    }
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        let mut writes = self.writes.lock().unwrap_or_else(|error| error.into_inner());
        let Some(count) = writes.get_mut(&self.identity) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            writes.remove(&self.identity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_counts_lifetime() {
        let writes = Registry::default();
        let identity = (4, 9);
        let first = WriteLease::acquire(identity, Arc::clone(&writes));
        let second = WriteLease::acquire(identity, Arc::clone(&writes));
        assert_eq!(writes.lock().unwrap().get(&identity), Some(&2));
        drop(first);
        assert_eq!(writes.lock().unwrap().get(&identity), Some(&1));
        drop(second);
        assert!(!writes.lock().unwrap().contains_key(&identity));
    }
}
