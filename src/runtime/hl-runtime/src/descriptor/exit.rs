use std::sync::Arc;

use hl_descriptor::DescriptorTable;
use hl_task::{ProcessId, ThreadId};

use crate::epoll::PreparedEpollExec;
use crate::{
    Control, DescriptorImageSlot, ExitParticipant, ExitRuntimeError, PreparedExecParticipant, PreparedExitParticipant,
    PreparedProcessImage,
};

/// Reversibly removes every descriptor and its epoll ownership before task exit.
pub struct Exit {
    descriptors: Arc<DescriptorImageSlot>,
    epoll: Arc<Control>,
}

impl Exit {
    #[must_use]
    pub fn new(descriptors: Arc<DescriptorImageSlot>, epoll: Arc<Control>) -> Self {
        Self { descriptors, epoll }
    }
}

impl ExitParticipant for Exit {
    fn prepare(&self, _: ProcessId, _: &[ThreadId]) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        let (descriptors, retired, previous) = self.descriptors.prepare_exit().map_err(|_| ExitRuntimeError::Failed)?;
        Ok(Box::new(PreparedDescriptorExit {
            descriptors,
            epoll: self.epoll.prepare_exec(retired),
            previous,
            descriptor_published: false,
            epoll_published: false,
        }))
    }
}

struct PreparedDescriptorExit {
    descriptors: PreparedProcessImage<Arc<DescriptorTable>>,
    epoll: PreparedEpollExec,
    previous: Arc<DescriptorTable>,
    descriptor_published: bool,
    epoll_published: bool,
}

impl PreparedExitParticipant for PreparedDescriptorExit {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        self.descriptors.publish().map_err(|_| ExitRuntimeError::Failed)?;
        self.descriptor_published = true;
        if !self.epoll.publish() {
            self.descriptors.rollback();
            self.descriptor_published = false;
            return Err(ExitRuntimeError::Failed);
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
        for descriptor in self.previous.active_snapshots() {
            let _ = self.previous.close(descriptor.number);
        }
        self.epoll_published = false;
        self.descriptor_published = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hl_descriptor::{DescriptorFlags, DescriptorTable};
    use hl_event::EpollInterest;
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

    use crate::RuntimeDescriptorTable;

    struct Fixture {
        control: Arc<Control>,
        table: Arc<RuntimeDescriptorTable>,
        slot: Arc<DescriptorImageSlot>,
        process: ProcessId,
    }

    impl Fixture {
        fn new() -> Self {
            let descriptors = Arc::new(DescriptorTable::new(16).unwrap());
            let (control, table) = Control::attach(Arc::clone(&descriptors), 16, 16).unwrap();
            let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
            let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
            let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
            Self {
                control: Arc::new(control),
                table: Arc::new(table),
                slot: Arc::new(DescriptorImageSlot::from_shared(descriptors)),
                process,
            }
        }

        fn populate(&self) {
            let source = self
                .control
                .create_epoll(&self.table, DescriptorFlags::default())
                .unwrap();
            let target = self
                .control
                .create_epoll(&self.table, DescriptorFlags::default())
                .unwrap();
            self.control
                .add(
                    &self.table,
                    source,
                    target,
                    EpollInterest::from_bits(EpollInterest::READ),
                    1,
                )
                .unwrap();
        }

        fn participant(&self) -> Exit {
            Exit::new(Arc::clone(&self.slot), Arc::clone(&self.control))
        }
    }

    #[test]
    fn publish_then_rollback() {
        let fixture = Fixture::new();
        fixture.populate();
        let before = fixture.control.graph_snapshot();
        let mut stage = fixture.participant().prepare(fixture.process, &[]).unwrap();

        stage.publish().unwrap();
        assert!(fixture.slot.current().1.active_snapshots().is_empty());
        assert!(fixture.control.graph_snapshot().edges.is_empty());

        stage.rollback();
        assert_eq!(fixture.slot.current().1.active_snapshots().len(), 2);
        assert_eq!(fixture.control.graph_snapshot(), before);
    }

    #[test]
    fn finish_commits_exit() {
        let fixture = Fixture::new();
        fixture.populate();
        let mut stage = fixture.participant().prepare(fixture.process, &[]).unwrap();

        stage.publish().unwrap();
        stage.finish();

        assert!(fixture.slot.current().1.active_snapshots().is_empty());
        assert!(fixture.control.graph_snapshot().edges.is_empty());
    }

    #[test]
    fn stale_generation_safe() {
        let fixture = Fixture::new();
        fixture.populate();
        let before = fixture.control.graph_snapshot();
        let mut exit = fixture.participant().prepare(fixture.process, &[]).unwrap();
        let (generation, _) = fixture.slot.current();
        let mut competing = fixture.slot.prepare(generation);
        competing.publish().unwrap();

        assert_eq!(exit.publish(), Err(ExitRuntimeError::Failed));
        assert_eq!(fixture.control.graph_snapshot(), before);
        assert_eq!(fixture.slot.current().1.active_snapshots().len(), 2);
    }
}
