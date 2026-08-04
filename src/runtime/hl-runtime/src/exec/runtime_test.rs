use std::sync::{Arc, Mutex};

use hl_linux::ExecPlan;
use hl_task::{ProcessCredentials, ProcessId, ProcessLimits, RegistryConfig, TaskRegistry, ThreadId};

use crate::{
    ExecRole, ExecRuntime, ExecRuntimeDependencies, PreparedExecParticipant, RuntimeExecError, RuntimeExecParticipant,
    RuntimeExecPort,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Prepare(usize),
    Publish(usize),
    Rollback(usize),
    Finish(usize),
}

struct Participant {
    identifier: usize,
    failed_publish: Option<usize>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl RuntimeExecParticipant for Participant {
    fn prepare(
        &self,
        _: ProcessId,
        _: ThreadId,
        _: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        self.events.lock().unwrap().push(Event::Prepare(self.identifier));
        Ok(Box::new(Stage {
            identifier: self.identifier,
            failed_publish: self.failed_publish,
            events: Arc::clone(&self.events),
        }))
    }
}

struct Stage {
    identifier: usize,
    failed_publish: Option<usize>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl PreparedExecParticipant for Stage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.events.lock().unwrap().push(Event::Publish(self.identifier));
        if self.failed_publish == Some(self.identifier) {
            return Err(RuntimeExecError::Failed);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.events.lock().unwrap().push(Event::Rollback(self.identifier));
    }

    fn finish(&mut self) {
        self.events.lock().unwrap().push(Event::Finish(self.identifier));
    }
}

fn participant(
    identifier: usize,
    failed_publish: Option<usize>,
    events: &Arc<Mutex<Vec<Event>>>,
) -> Arc<dyn RuntimeExecParticipant> {
    Arc::new(Participant {
        identifier,
        failed_publish,
        events: Arc::clone(events),
    })
}

struct RuntimeFixture;

impl RuntimeFixture {
    fn dependencies(failed_publish: Option<usize>, events: &Arc<Mutex<Vec<Event>>>) -> ExecRuntimeDependencies {
        ExecRuntimeDependencies::builder()
            .participant(ExecRole::Task, participant(3, failed_publish, events))
            .unwrap()
            .participant(ExecRole::DescriptorEpoll, participant(1, failed_publish, events))
            .unwrap()
            .participant(ExecRole::Ipc, participant(2, failed_publish, events))
            .unwrap()
            .participant(ExecRole::Loader, participant(0, failed_publish, events))
            .unwrap()
            .build()
            .unwrap()
    }

    fn plan() -> ExecPlan {
        ExecPlan {
            directory: None,
            path: b"/bin/program".to_vec(),
            arguments: vec![b"program".to_vec()],
            environment: Vec::new(),
            flags: 0,
        }
    }
}

fn identity() -> (ProcessId, ThreadId) {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
    tasks.create_init(credentials, ProcessLimits::empty()).unwrap()
}

#[test]
fn dependencies_duplicate_roles() {
    let events = Arc::new(Mutex::new(Vec::new()));
    assert!(matches!(
        ExecRuntimeDependencies::builder()
            .participant(ExecRole::Task, participant(3, None, &events),)
            .unwrap()
            .build(),
        Err(RuntimeExecError::Unsupported),
    ));
    assert!(matches!(
        ExecRuntimeDependencies::builder()
            .participant(ExecRole::Task, participant(3, None, &events),)
            .unwrap()
            .participant(ExecRole::Task, participant(4, None, &events),),
        Err(RuntimeExecError::Invalid),
    ));
}

#[test]
fn role_roles_reverse() {
    let prepare_order = [3, 1, 2, 0];
    let publish_order = [2, 1, 0, 3];
    for failed in 0..4 {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = ExecRuntime::new(RuntimeFixture::dependencies(Some(failed), &events)).unwrap();
        let (process, thread) = identity();
        assert_eq!(
            runtime.exec(process, thread, RuntimeFixture::plan()),
            Err(RuntimeExecError::Failed),
        );
        let mut expected = prepare_order.into_iter().map(Event::Prepare).collect::<Vec<_>>();
        expected.extend(
            publish_order
                .into_iter()
                .take_while(|identifier| *identifier != failed)
                .chain(std::iter::once(failed))
                .map(Event::Publish),
        );
        expected.extend(publish_order.into_iter().rev().map(Event::Rollback));
        assert_eq!(*events.lock().unwrap(), expected);
    }
}

#[test]
fn linux_roles_publish() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let runtime = ExecRuntime::new(RuntimeFixture::dependencies(None, &events)).unwrap();
    let (process, thread) = identity();
    runtime.exec(process, thread, RuntimeFixture::plan()).unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            Event::Prepare(3),
            Event::Prepare(1),
            Event::Prepare(2),
            Event::Prepare(0),
            Event::Publish(2),
            Event::Publish(1),
            Event::Publish(0),
            Event::Publish(3),
            Event::Finish(3),
            Event::Finish(1),
            Event::Finish(2),
            Event::Finish(0),
        ],
    );
}
