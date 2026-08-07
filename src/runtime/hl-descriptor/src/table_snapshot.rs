use std::sync::atomic::Ordering;

use std::sync::Arc;

use crate::model::OpenDescription;
use crate::{DescriptorFlags, DescriptorSnapshot, DescriptorTable, StatusFlags};

/// Hard limits for one atomic descriptor-table snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotBudget {
    pub max_items: usize,
    /// Maximum peak bytes reserved by the two descriptor-owned vectors.
    pub max_peak_bytes: usize,
}

/// Failure to size a bounded descriptor-table snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    /// The snapshot exceeds either caller-supplied bound.
    Limit,
    /// Peak vector storage is not representable by `usize`.
    Overflow,
}

fn peak_bytes(count: usize) -> Option<usize> {
    let admitted = count.checked_mul(std::mem::size_of::<AdmittedSnapshot>())?;
    let result = count.checked_mul(std::mem::size_of::<DescriptorSnapshot>())?;
    admitted.checked_add(result)
}

fn admit_count(count: usize, budget: SnapshotBudget) -> Result<(), SnapshotError> {
    if count > budget.max_items {
        return Err(SnapshotError::Limit);
    }
    let peak_bytes = peak_bytes(count).ok_or(SnapshotError::Overflow)?;
    if peak_bytes > budget.max_peak_bytes {
        return Err(SnapshotError::Limit);
    }
    Ok(())
}

struct AdmittedSnapshot {
    number: i32,
    description: Arc<OpenDescription>,
    offset: u64,
    status: StatusFlags,
    flags: DescriptorFlags,
    descriptor_generation: u32,
    description_generation: u32,
    descriptor_references: u32,
}

impl DescriptorTable {
    #[cfg(test)]
    fn observe_snapshot_read_lock(&self, observe: impl FnOnce()) {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        observe();
        drop(state);
    }

    /// Captures every active descriptor without changing table or OFD state.
    #[must_use]
    pub fn active_snapshots(&self) -> Vec<DescriptorSnapshot> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .iter()
            .map(|(number, descriptor)| {
                let description_state = descriptor
                    .description
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                DescriptorSnapshot {
                    number: *number,
                    description_identity: descriptor.description.identity,
                    offset: description_state.offset,
                    status: description_state.status,
                    flags: descriptor.flags,
                    descriptor_generation: descriptor.generation,
                    description_generation: descriptor.description.generation,
                    descriptor_references: descriptor.description.descriptor_references.load(Ordering::Acquire),
                    kind: descriptor.description.object.kind(),
                    flock_token: descriptor.description.identity,
                }
            })
            .collect()
    }

    /// Atomically captures every active descriptor within caller-owned bounds.
    ///
    /// Sizing and admission copying occur under the same table read lock. Consequently a
    /// successful result represents one table generation, while a limit or
    /// overflow failure allocates no result storage and returns no partial data.
    /// Preflight uses only fixed-width scalar arithmetic: this API accepts no
    /// callback or object method that could re-enter the table while it is locked.
    ///
    /// # Errors
    ///
    /// The byte bound is the mechanically computed peak capacity of the admitted
    /// and result vectors. During that peak, admitted entries also pin each live
    /// open description with an `Arc`; cloning those `Arc`s does not allocate and
    /// the already-owned descriptions and their targets are not charged here.
    /// Returns [`SnapshotError::Limit`] if either budget is exceeded and
    /// [`SnapshotError::Overflow`] if peak-capacity accounting overflows.
    pub fn bounded_active_snapshots(&self, budget: SnapshotBudget) -> Result<Vec<DescriptorSnapshot>, SnapshotError> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = state.entries.len();
        admit_count(count, budget)?;

        let mut admitted = Vec::with_capacity(count);
        for (number, descriptor) in &state.entries {
            let description_state = descriptor
                .description
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            admitted.push(AdmittedSnapshot {
                number: *number,
                description: descriptor.description.clone(),
                offset: description_state.offset,
                status: description_state.status,
                flags: descriptor.flags,
                descriptor_generation: descriptor.generation,
                description_generation: descriptor.description.generation,
                descriptor_references: descriptor.description.descriptor_references.load(Ordering::Acquire),
            });
        }
        debug_assert_eq!(admitted.len(), admitted.capacity());
        drop(state);

        let mut snapshots = Vec::with_capacity(count);
        for entry in admitted {
            let identity = entry.description.identity;
            let kind = entry.description.object.kind();
            snapshots.push(DescriptorSnapshot {
                number: entry.number,
                description_identity: identity,
                offset: entry.offset,
                status: entry.status,
                flags: entry.flags,
                descriptor_generation: entry.descriptor_generation,
                description_generation: entry.description_generation,
                descriptor_references: entry.descriptor_references,
                kind,
                flock_token: identity,
            });
        }
        debug_assert_eq!(snapshots.len(), snapshots.capacity());
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;

    use super::{SnapshotBudget, SnapshotError};
    use crate::{DescriptorFlags, DescriptorTable, OpenFileDescription};

    #[derive(Debug)]
    struct Object;
    impl OpenFileDescription for Object {}

    fn populated() -> DescriptorTable {
        let table = DescriptorTable::new(8).unwrap();
        table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
        table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
        table
    }

    #[test]
    fn exact_bounds_empty_and_overflow_have_no_partial_result() {
        let empty = DescriptorTable::new(0).unwrap();
        assert_eq!(
            empty
                .bounded_active_snapshots(SnapshotBudget {
                    max_items: 0,
                    max_peak_bytes: 0
                })
                .unwrap(),
            Vec::new()
        );

        let table = populated();
        let exact = table
            .bounded_active_snapshots(SnapshotBudget {
                max_items: 2,
                max_peak_bytes: super::peak_bytes(2).unwrap(),
            })
            .unwrap();
        assert_eq!(exact.len(), 2);
        assert_eq!(exact.capacity(), 2);
        assert_eq!(
            table.bounded_active_snapshots(SnapshotBudget {
                max_items: 1,
                max_peak_bytes: super::peak_bytes(2).unwrap()
            },),
            Err(SnapshotError::Limit)
        );
        assert_eq!(
            super::admit_count(
                usize::MAX,
                SnapshotBudget {
                    max_items: usize::MAX,
                    max_peak_bytes: usize::MAX,
                }
            ),
            Err(SnapshotError::Overflow)
        );
        assert_eq!(
            table.bounded_active_snapshots(SnapshotBudget {
                max_items: 2,
                max_peak_bytes: super::peak_bytes(2).unwrap() - 1
            },),
            Err(SnapshotError::Limit)
        );
    }

    #[test]
    fn close_and_reuse_preserves_order_identity_and_generation() {
        let table = populated();
        let before = table
            .bounded_active_snapshots(SnapshotBudget {
                max_items: 2,
                max_peak_bytes: super::peak_bytes(2).unwrap(),
            })
            .unwrap();
        table.close(0).unwrap();
        table.install(0, Arc::new(Object), DescriptorFlags::default()).unwrap();
        let after = table
            .bounded_active_snapshots(SnapshotBudget {
                max_items: 2,
                max_peak_bytes: super::peak_bytes(2).unwrap(),
            })
            .unwrap();
        assert_eq!(after.iter().map(|entry| entry.number).collect::<Vec<_>>(), vec![0, 1]);
        assert_ne!(before[0].description_identity, after[0].description_identity);
        assert!(after[0].descriptor_generation > before[0].descriptor_generation);
    }

    #[test]
    fn read_lock_blocks_known_writer_attempt_then_releases() {
        let table = Arc::new(populated());
        let start = Arc::new(Barrier::new(2));
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        let writer_table = table.clone();
        let writer_start = start.clone();
        let writer = thread::spawn(move || {
            writer_start.wait();
            blocked_tx.send(writer_table.state.try_write().is_err()).unwrap();
            release_rx.recv().unwrap();
            writer_table.close(0).unwrap();
            completed_tx.send(()).unwrap();
        });

        table.observe_snapshot_read_lock(|| {
            start.wait();
            assert!(blocked_rx.recv().unwrap());
        });
        release_tx.send(()).unwrap();
        completed_rx.recv().unwrap();
        writer.join().unwrap();
        assert_eq!(table.active_snapshots().len(), 1);
    }

    #[test]
    fn budget_api_has_no_reentrant_callback_parameter() {
        let capture: fn(&DescriptorTable, SnapshotBudget) -> Result<Vec<crate::DescriptorSnapshot>, SnapshotError> =
            DescriptorTable::bounded_active_snapshots;
        let _ = capture;
    }
}
