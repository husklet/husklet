use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_checkpoint::{CheckpointImage, Section};
use hl_execution::{EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionMachine, ExecutionSnapshot};

use crate::{CheckpointParticipant, CheckpointRole};

const DEPENDENCIES: [CheckpointRole; 1] = [CheckpointRole::Memory];

struct ExecutionRestore {
    previous: ExecutionSnapshot,
    replacement: ExecutionSnapshot,
    committed: bool,
}

pub struct ExecutionCheckpointParticipant {
    machine: Arc<ExecutionMachine>,
    staged: Mutex<BTreeMap<u64, ExecutionRestore>>,
    next: AtomicU64,
}

impl ExecutionCheckpointParticipant {
    #[must_use]
    pub fn new(machine: Arc<ExecutionMachine>) -> Self {
        Self {
            machine,
            staged: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }
}

impl CheckpointParticipant for ExecutionCheckpointParticipant {
    fn role(&self) -> CheckpointRole {
        CheckpointRole::Execution
    }

    fn version(&self) -> u32 {
        EXECUTION_SNAPSHOT_VERSION
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &DEPENDENCIES
    }

    fn freeze(&self) -> Result<(), ()> {
        self.machine.freeze().map_err(|_| ())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        let mut snapshot = self.machine.snapshot().map_err(|_| ())?;
        if let ExecutionCpuSnapshot::Aarch64(cpu) = &mut snapshot.cpu {
            // A local exclusive monitor is permitted to be cleared by context
            // events and has no durable memory identity. Restoring it would
            // allow a stale STXR to cross the checkpoint boundary.
            cpu.clear_exclusive_reservation();
        }
        snapshot.encode().map_err(|_| ())
    }

    fn thaw(&self) -> Result<(), ()> {
        self.machine.thaw().map_err(|_| ())
    }

    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        let replacement = ExecutionSnapshot::decode(section.bytes()).map_err(|_| ())?;
        self.machine.freeze().map_err(|_| ())?;
        let architecture = self.machine.snapshot().map_err(|_| ())?.architecture();
        self.machine.thaw().map_err(|_| ())?;
        if replacement.architecture() != architecture {
            return Err(());
        }
        Ok(())
    }

    fn stage(&self, section: &Section) -> Result<u64, ()> {
        self.machine.freeze().map_err(|_| ())?;
        let result = (|| {
            let previous = self.machine.snapshot().map_err(|_| ())?;
            let replacement = ExecutionSnapshot::decode(section.bytes()).map_err(|_| ())?;
            if previous.architecture() != replacement.architecture() {
                return Err(());
            }
            let reservation = self.next.fetch_add(1, Ordering::Relaxed);
            if reservation == 0 {
                return Err(());
            }
            self.staged.lock().map_err(|_| ())?.insert(
                reservation,
                ExecutionRestore {
                    previous,
                    replacement,
                    committed: false,
                },
            );
            Ok(reservation)
        })();
        if result.is_err() {
            let _ = self.machine.thaw();
        }
        result
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        let mut staged = self.staged.lock().map_err(|_| ())?;
        let state = staged.get_mut(&reservation).ok_or(())?;
        self.machine.replace(state.replacement.clone()).map_err(|_| ())?;
        state.committed = true;
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let state = self
            .staged
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&reservation);
        if let Some(state) = state {
            if state.committed {
                let _ = self.machine.replace(state.previous);
            }
            let _ = self.machine.thaw();
        }
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        let state = self.staged.lock().map_err(|_| ())?.remove(&reservation).ok_or(())?;
        if !state.committed {
            return Err(());
        }
        self.machine.thaw().map_err(|_| ())
    }
}
