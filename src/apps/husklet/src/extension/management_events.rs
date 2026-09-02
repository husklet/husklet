//! Bounded native changes behind the extension subscription topic.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use hl_extension::port::ExtensionSummary;

use super::acquisition::{AcquisitionJob, AcquisitionSnapshot};

/// A coalesced batch ready for the Conversation subscription adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExtensionEventBatch {
    pub revision: u64,
    pub inventory: Option<Vec<ExtensionSummary>>,
    pub acquisitions: Vec<AcquisitionInvalidation>,
    pub dropped: u64,
}

/// Latest state of one acquisition job, not an unbounded event history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcquisitionInvalidation {
    pub job: String,
    pub snapshot: AcquisitionSnapshot,
}

#[derive(Default)]
struct Pending {
    revision: u64,
    inventory: Option<Vec<ExtensionSummary>>,
    acquisitions: BTreeMap<AcquisitionJob, AcquisitionSnapshot>,
    dropped: u64,
}

/// Shared producer for inventory and acquisition invalidations.
///
/// Inventory always replaces inventory. Per-job acquisition changes replace
/// the older revision for that job. At most 32 jobs are pending, so a stalled
/// subscriber cannot turn native progress into unbounded memory.
#[derive(Clone, Default)]
pub(crate) struct ExtensionEvents {
    pending: Arc<Mutex<Pending>>,
}

impl ExtensionEvents {
    pub(crate) const JOB_LIMIT: usize = 32;

    pub(crate) fn inventory(&self, entries: Vec<ExtensionSummary>) {
        let mut pending = self.lock();
        pending.revision = pending.revision.saturating_add(1);
        pending.inventory = Some(entries);
    }

    pub(crate) fn acquisition(&self, job: AcquisitionJob, snapshot: AcquisitionSnapshot) {
        let mut pending = self.lock();
        pending.revision = pending.revision.saturating_add(1);
        if !pending.acquisitions.contains_key(&job) && pending.acquisitions.len() == Self::JOB_LIMIT {
            if let Some(oldest) = pending
                .acquisitions
                .iter()
                .min_by_key(|(_, snapshot)| snapshot.revision)
                .map(|(job, _)| *job)
            {
                pending.acquisitions.remove(&oldest);
                pending.dropped = pending.dropped.saturating_add(1);
            }
        }
        pending.acquisitions.insert(job, snapshot);
    }

    /// Drains one bounded coalesced batch. `None` means nothing changed.
    pub(crate) fn drain(&self) -> Option<ExtensionEventBatch> {
        let mut pending = self.lock();
        if pending.inventory.is_none() && pending.acquisitions.is_empty() && pending.dropped == 0 {
            return None;
        }
        Some(ExtensionEventBatch {
            revision: pending.revision,
            inventory: pending.inventory.take(),
            acquisitions: std::mem::take(&mut pending.acquisitions)
                .into_iter()
                .map(|(job, snapshot)| AcquisitionInvalidation {
                    job: job.wire(),
                    snapshot,
                })
                .collect(),
            dropped: std::mem::take(&mut pending.dropped),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::acquisition::AcquisitionState;

    fn snapshot(reference: &str, revision: u64) -> AcquisitionSnapshot {
        AcquisitionSnapshot {
            reference: reference.into(),
            revision,
            state: AcquisitionState::Inspecting,
        }
    }

    #[test]
    fn inventory_and_each_job_coalesce_to_the_latest_truth() {
        let events = ExtensionEvents::default();
        events.inventory(vec![ExtensionSummary {
            name: "old".into(),
            image_digest: "sha256:old".into(),
            status: "standby".into(),
        }]);
        events.inventory(vec![ExtensionSummary {
            name: "new".into(),
            image_digest: "sha256:new".into(),
            status: "duty".into(),
        }]);
        let job = AcquisitionJob::test(7);
        events.acquisition(job, snapshot("first", 1));
        events.acquisition(job, snapshot("latest", 2));

        let batch = events.drain().unwrap();
        assert_eq!(batch.inventory.unwrap()[0].name, "new");
        assert_eq!(batch.acquisitions.len(), 1);
        assert_eq!(batch.acquisitions[0].snapshot.reference, "latest");
        assert!(events.drain().is_none());
    }

    #[test]
    fn pending_jobs_are_bounded_and_loss_is_visible() {
        let events = ExtensionEvents::default();
        for index in 0..=ExtensionEvents::JOB_LIMIT {
            events.acquisition(
                AcquisitionJob::test(index as u64 + 1),
                snapshot("image", index as u64 + 1),
            );
        }
        let batch = events.drain().unwrap();
        assert_eq!(batch.acquisitions.len(), ExtensionEvents::JOB_LIMIT);
        assert_eq!(batch.dropped, 1);
    }
}
