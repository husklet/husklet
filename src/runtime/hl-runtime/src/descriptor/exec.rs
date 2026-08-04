use std::collections::BTreeSet;
use std::sync::Arc;

use hl_descriptor::{DescriptionIdentity, DescriptorSnapshot, DescriptorTable};
use hl_linux::ExecPlan;
use hl_task::{ProcessId, ThreadId};

use crate::epoll::PreparedEpollExec;
use crate::{
    Control, PreparedExecParticipant, PreparedProcessImage, ProcessImage, RuntimeExecError, RuntimeExecParticipant,
};

/// Generation-checked owner of the descriptor table published for one process.
///
/// Exec preparation forks the live table so descriptor numbers and generations
/// are copied while open file descriptions remain shared. Close-on-exec is then
/// applied only to the unpublished candidate.
pub struct ImageSlot {
    image: ProcessImage<Arc<DescriptorTable>>,
}

impl ImageSlot {
    #[must_use]
    pub fn new(table: DescriptorTable) -> Self {
        Self {
            image: ProcessImage::new(Arc::new(table)),
        }
    }

    #[must_use]
    pub fn from_shared(table: Arc<DescriptorTable>) -> Self {
        Self {
            image: ProcessImage::new(table),
        }
    }

    /// Returns the generation and table that are currently published.
    #[must_use]
    pub fn current(&self) -> (u64, Arc<DescriptorTable>) {
        let (generation, table) = self.image.current();
        (generation, table.as_ref().clone())
    }

    /// Stages close-on-exec against a private fork of the expected image.
    ///
    /// Publication fails if another image was published after `expected` was
    /// observed.
    #[must_use]
    pub fn prepare(&self, expected: u64) -> PreparedProcessImage<Arc<DescriptorTable>> {
        let (_, current) = self.current();
        let candidate = current.fork();
        candidate.close_on_exec();
        self.image.prepare(expected, Arc::new(candidate))
    }

    /// Stages an exact checkpoint replacement against the observed generation.
    #[must_use]
    pub(crate) fn prepare_checkpoint(
        &self,
        expected: u64,
        replacement: Arc<DescriptorTable>,
    ) -> PreparedProcessImage<Arc<DescriptorTable>> {
        self.image.prepare(expected, replacement)
    }

    pub(crate) fn prepare_exit(
        &self,
    ) -> Result<
        (
            PreparedProcessImage<Arc<DescriptorTable>>,
            BTreeSet<DescriptionIdentity>,
            Arc<DescriptorTable>,
        ),
        RuntimeExecError,
    > {
        let (generation, current) = self.current();
        let candidate = current.fork();
        let snapshots = candidate.active_snapshots();
        let retired = Exec::identities(snapshots.clone());
        for descriptor in snapshots {
            candidate
                .close(descriptor.number)
                .map_err(|_| RuntimeExecError::Failed)?;
        }
        Ok((self.image.prepare(generation, Arc::new(candidate)), retired, current))
    }
}

pub struct Exec {
    descriptors: Arc<ImageSlot>,
    epoll: Arc<Control>,
}

impl Exec {
    #[must_use]
    pub fn new(descriptors: Arc<ImageSlot>, epoll: Arc<Control>) -> Self {
        Self { descriptors, epoll }
    }

    pub fn prepare_current(&self) -> Result<PreparedDescriptorExec, RuntimeExecError> {
        let (generation, current) = self.descriptors.current();
        let candidate = current.fork();
        candidate.close_on_exec();
        let retired = Self::identities(current.active_snapshots())
            .difference(&Self::identities(candidate.active_snapshots()))
            .copied()
            .collect();
        Ok(PreparedDescriptorExec {
            descriptors: self.descriptors.image.prepare(generation, Arc::new(candidate)),
            epoll: self.epoll.prepare_exec(retired),
            descriptor_published: false,
            epoll_published: false,
        })
    }
}

pub struct PreparedDescriptorExec {
    descriptors: PreparedProcessImage<Arc<DescriptorTable>>,
    epoll: PreparedEpollExec,
    descriptor_published: bool,
    epoll_published: bool,
}

impl PreparedDescriptorExec {
    #[must_use]
    pub fn candidate(&self) -> Option<Arc<DescriptorTable>> {
        self.descriptors.candidate().map(|table| Arc::clone(table.as_ref()))
    }
}

impl RuntimeExecParticipant for Exec {
    fn prepare(
        &self,
        _: ProcessId,
        _: ThreadId,
        _: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        self.prepare_current()
            .map(|prepared| Box::new(prepared) as Box<dyn PreparedExecParticipant>)
    }
}

impl Exec {
    fn identities(snapshots: Vec<DescriptorSnapshot>) -> BTreeSet<DescriptionIdentity> {
        snapshots
            .into_iter()
            .map(|snapshot| DescriptionIdentity {
                identity: snapshot.description_identity,
                generation: snapshot.description_generation,
            })
            .collect()
    }
}

impl PreparedExecParticipant for PreparedDescriptorExec {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.descriptors.publish()?;
        self.descriptor_published = true;
        if !self.epoll.publish() {
            self.descriptors.rollback();
            self.descriptor_published = false;
            return Err(RuntimeExecError::Failed);
        }
        self.epoll_published = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if self.epoll_published {
            self.epoll.rollback();
            self.epoll_published = false;
        }
        if self.descriptor_published {
            self.descriptors.rollback();
            self.descriptor_published = false;
        }
    }

    fn finish(&mut self) {
        self.epoll.finish();
        self.descriptors.finish();
        self.epoll_published = false;
        self.descriptor_published = false;
    }
}
