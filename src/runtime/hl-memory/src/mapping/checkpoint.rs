use super::{Coordinator, Host};
use crate::{MemoryError, SharedError};

impl<H: Host> Coordinator<H> {
    pub fn with_frozen_snapshot<R>(
        &self,
        snapshot: impl FnOnce(&crate::FrozenSnapshotAuthority, crate::MemoryLedgerSnapshot) -> R,
    ) -> R {
        self.activity.freeze();
        drop(self.transaction.lock().unwrap_or_else(|error| error.into_inner()));
        struct Thaw<'a>(&'a crate::CheckpointActivity);
        impl Drop for Thaw<'_> {
            fn drop(&mut self) {
                self.0.thaw();
            }
        }
        let _thaw = Thaw(&self.activity);
        let authority = crate::FrozenSnapshotAuthority { _private: () };
        snapshot(&authority, self.ledger.snapshot())
    }

    pub fn checkpoint_snapshot(&self) -> Result<crate::MemoryLedgerSnapshot, MemoryError> {
        if !self.activity.frozen() {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(self.ledger.snapshot())
    }

    pub fn checkpoint_image(
        &self,
        host: &dyn crate::MemoryCheckpointHost<H>,
    ) -> Result<crate::MemoryCheckpointImage, MemoryError> {
        if !self.activity.frozen() {
            return Err(MemoryError::InvariantViolation);
        }
        let shared = self.shared.as_ref().ok_or(MemoryError::Shared(SharedError::NotFound))?;
        let ledger = self.checkpoint_snapshot()?;
        let shared_snapshot = shared.checkpoint_snapshot()?;
        let authority = crate::FrozenSnapshotAuthority { _private: () };
        let mappings = ledger
            .regions
            .iter()
            .copied()
            .enumerate()
            .map(|(region, value)| {
                host.snapshot_mapping(&authority, value)
                    .map(|bytes| crate::MemoryMappingSnapshot { region, bytes })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let image = crate::MemoryCheckpointImage {
            version: crate::MEMORY_CHECKPOINT_VERSION,
            address_limit: host.address_limit(),
            shared_limits: shared.limits(),
            shared: shared_snapshot,
            ledger,
            mappings,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.transaction.lock().unwrap_or_else(|error| error.into_inner()));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }
}
