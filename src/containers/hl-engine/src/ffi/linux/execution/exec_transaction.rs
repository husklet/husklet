//! Staged exec transaction publication and rollback.

use std::sync::{Arc, Mutex};

use hl_execution::ExecutionSnapshot;
use hl_loader::InitialTlsPlan;
use hl_runtime::{PreparedDescriptorExec, PreparedExec, PreparedExecParticipant, PreparedLoaderExec, RuntimeExecError};

use super::exec_image::ImageSpace;

pub(super) struct Transaction {
    pub(super) descriptors: PreparedDescriptorExec,
    pub(super) loader: PreparedLoaderExec<ImageSpace, InitialTlsPlan, ExecutionSnapshot>,
    pub(super) threads: super::threads::PreparedImage,
    pub(super) retire: super::exec_retire::RetireImage,
    pub(super) tasks: Box<dyn PreparedExecParticipant>,
    pub(super) ipc: Box<dyn PreparedExecParticipant>,
    pub(super) active: Arc<Mutex<bool>>,
    pub(super) complete: bool,
    pub(super) process: Arc<super::routing::ProcessContext>,
    pub(super) process_slot: Arc<Mutex<std::sync::Weak<super::routing::ProcessContext>>>,
    pub(super) identity: Arc<Mutex<Vec<u8>>>,
    pub(super) executable: Vec<u8>,
    pub(super) auxiliary_slot: Arc<Mutex<Vec<u8>>>,
    pub(super) auxiliary: Vec<u8>,
    pub(super) previous_auxiliary: Option<Vec<u8>>,
    pub(super) vfork: Option<Arc<hl_runtime::VforkParentToken>>,
}

impl Transaction {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.ipc.publish()?;
        self.descriptors.publish()?;
        self.loader.publish()?;
        let mut auxiliary = self.auxiliary_slot.lock().map_err(|_| RuntimeExecError::Failed)?;
        self.previous_auxiliary = Some(std::mem::replace(&mut *auxiliary, self.auxiliary.clone()));
        drop(auxiliary);
        self.threads.publish().map_err(|_| RuntimeExecError::Failed)?;
        self.retire.publish()?;
        self.tasks.publish()?;
        self.process.publish_procfs();
        *self.process_slot.lock().map_err(|_| RuntimeExecError::Failed)? = Arc::downgrade(&self.process);
        *self.identity.lock().map_err(|_| RuntimeExecError::Failed)? = self.executable.clone();
        Ok(())
    }

    fn rollback(&mut self) {
        self.tasks.rollback();
        self.retire.rollback();
        self.threads.rollback();
        if let Some(previous) = self.previous_auxiliary.take() {
            *self
                .auxiliary_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = previous;
        }
        self.loader.rollback();
        self.descriptors.rollback();
        self.ipc.rollback();
    }

    fn finish(&mut self) {
        self.retire.finish();
        self.ipc.finish();
        self.descriptors.finish();
        self.loader.finish();
        self.threads.finish();
        self.tasks.finish();
        self.previous_auxiliary = None;
    }

    fn release(&self) {
        *self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }
}

impl PreparedExec for Transaction {
    fn commit(mut self: Box<Self>) -> Result<(), RuntimeExecError> {
        if let Err(error) = self.publish() {
            self.rollback();
            self.complete = true;
            self.release();
            return Err(error);
        }
        if let Some(token) = &self.vfork {
            let _ = token.release(self.process.process());
        }
        self.finish();
        self.complete = true;
        self.release();
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.complete {
            self.rollback();
            self.release();
        }
    }
}
