use std::sync::{Arc, Mutex};

use hl_linux::ExecPlan;
use hl_task::{ProcessCredentials, ProcessId, ProcessLimits, RegistryConfig, TaskRegistry, ThreadId};

use crate::{
    ExecKey, ExecQueue, PreparedExecParticipant, ProcessImage, RuntimeExecError, RuntimeExecParticipant,
    RuntimeExecPort, SafeRuntimeExec,
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
    fail_prepare: bool,
    fail_publish: bool,
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
        if self.fail_prepare {
            return Err(RuntimeExecError::Format);
        }
        Ok(Box::new(ExecStage {
            identifier: self.identifier,
            fail_publish: self.fail_publish,
            events: Arc::clone(&self.events),
        }))
    }
}

struct ExecStage {
    identifier: usize,
    fail_publish: bool,
    events: Arc<Mutex<Vec<Event>>>,
}

impl PreparedExecParticipant for ExecStage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        self.events.lock().unwrap().push(Event::Publish(self.identifier));
        if self.fail_publish {
            Err(RuntimeExecError::Failed)
        } else {
            Ok(())
        }
    }

    fn rollback(&mut self) {
        self.events.lock().unwrap().push(Event::Rollback(self.identifier));
    }

    fn finish(&mut self) {
        self.events.lock().unwrap().push(Event::Finish(self.identifier));
    }
}

struct Fixture {
    events: Arc<Mutex<Vec<Event>>>,
    process: ProcessId,
    thread: ThreadId,
}

impl Fixture {
    fn new() -> Self {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            process,
            thread,
        }
    }

    fn participant(
        &self,
        identifier: usize,
        fail_prepare: bool,
        fail_publish: bool,
    ) -> Arc<dyn RuntimeExecParticipant> {
        Arc::new(Participant {
            identifier,
            fail_prepare,
            fail_publish,
            events: Arc::clone(&self.events),
        })
    }

    fn plan() -> ExecPlan {
        ExecPlan {
            directory: None,
            path: b"/bin/program".to_vec(),
            arguments: vec![b"program".to_vec()],
            environment: vec![b"A=B".to_vec()],
            flags: 0,
        }
    }

    fn recorded(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

#[test]
fn prepare_stages_reverse() {
    for failed in 0..3 {
        let fixture = Fixture::new();
        let participants = (0..3)
            .map(|index| fixture.participant(index, index == failed, false))
            .collect();
        let exec = SafeRuntimeExec::new(participants).unwrap();
        assert_eq!(
            exec.exec(fixture.process, fixture.thread, Fixture::plan(),),
            Err(RuntimeExecError::Format),
        );
        let mut expected = (0..=failed).map(Event::Prepare).collect::<Vec<_>>();
        expected.extend((0..failed).rev().map(Event::Rollback));
        assert_eq!(fixture.recorded(), expected);
    }
}

#[test]
fn publish_stages_reverse() {
    for failed in 0..3 {
        let fixture = Fixture::new();
        let participants = (0..3)
            .map(|index| fixture.participant(index, false, index == failed))
            .collect();
        let exec = SafeRuntimeExec::new(participants).unwrap();
        assert_eq!(
            exec.exec(fixture.process, fixture.thread, Fixture::plan(),),
            Err(RuntimeExecError::Failed),
        );
        let mut expected = (0..3).map(Event::Prepare).collect::<Vec<_>>();
        expected.extend((0..=failed).map(Event::Publish));
        expected.extend((0..3).rev().map(Event::Rollback));
        assert_eq!(fixture.recorded(), expected);
    }
}

#[test]
fn successful_after_publication() {
    let fixture = Fixture::new();
    let exec = SafeRuntimeExec::new((0..3).map(|index| fixture.participant(index, false, false)).collect()).unwrap();
    exec.exec(fixture.process, fixture.thread, Fixture::plan()).unwrap();
    assert_eq!(
        fixture.recorded(),
        vec![
            Event::Prepare(0),
            Event::Prepare(1),
            Event::Prepare(2),
            Event::Publish(0),
            Event::Publish(1),
            Event::Publish(2),
            Event::Finish(0),
            Event::Finish(1),
            Event::Finish(2),
        ],
    );
}

#[test]
fn abandoned_rolls_back() {
    let fixture = Fixture::new();
    let exec = SafeRuntimeExec::new((0..3).map(|index| fixture.participant(index, false, false)).collect()).unwrap();
    drop(exec.prepare(fixture.process, fixture.thread, &Fixture::plan()).unwrap());
    assert_eq!(
        fixture.recorded(),
        vec![
            Event::Prepare(0),
            Event::Prepare(1),
            Event::Prepare(2),
            Event::Rollback(2),
            Event::Rollback(1),
            Event::Rollback(0),
        ],
    );
}

#[test]
fn transaction_serial() {
    let fixture = Fixture::new();
    let exec = SafeRuntimeExec::new(vec![fixture.participant(0, false, false)]).unwrap();
    let prepared = exec.prepare(fixture.process, fixture.thread, &Fixture::plan()).unwrap();
    assert!(matches!(
        exec.prepare(fixture.process, fixture.thread, &Fixture::plan()),
        Err(RuntimeExecError::Failed),
    ));
    drop(prepared);
    assert!(exec.prepare(fixture.process, fixture.thread, &Fixture::plan()).is_ok());
}

#[test]
fn queue_exact_once() {
    let fixture = Fixture::new();
    let exec = SafeRuntimeExec::new(vec![fixture.participant(0, false, false)]).unwrap();
    let queue = ExecQueue::default();
    let key = queue
        .stage(
            fixture.thread,
            Box::new(exec.prepare(fixture.process, fixture.thread, &Fixture::plan()).unwrap()),
        )
        .unwrap();
    assert_eq!(queue.current(fixture.thread), Some(key));
    assert!(
        queue
            .take(ExecKey {
                generation: key.generation + 1,
                ..key
            })
            .is_none()
    );
    queue.take(key).unwrap().commit().unwrap();
    assert!(queue.take(key).is_none());
}

#[test]
fn queue_busy_rollback() {
    let fixture = Fixture::new();
    let first = SafeRuntimeExec::new(vec![fixture.participant(0, false, false)]).unwrap();
    let second = SafeRuntimeExec::new(vec![fixture.participant(1, false, false)]).unwrap();
    let queue = ExecQueue::default();
    let key = queue
        .stage(
            fixture.thread,
            Box::new(
                first
                    .prepare(fixture.process, fixture.thread, &Fixture::plan())
                    .unwrap(),
            ),
        )
        .unwrap();
    let prepared = second
        .prepare(fixture.process, fixture.thread, &Fixture::plan())
        .unwrap();
    assert!(matches!(
        queue.stage(fixture.thread, Box::new(prepared)),
        Err(RuntimeExecError::Failed),
    ));
    drop(queue.take(key));
    assert_eq!(
        fixture.recorded(),
        vec![
            Event::Prepare(0),
            Event::Prepare(1),
            Event::Rollback(1),
            Event::Rollback(0),
        ],
    );
}

#[test]
fn old_image() {
    let image = ProcessImage::new(String::from("old"));
    let (generation, old) = image.current();
    let mut replacement = image.prepare(generation, String::from("new"));
    replacement.publish().unwrap();
    assert_eq!(image.current().1.as_str(), "new");
    replacement.rollback();
    let (restored_generation, restored) = image.current();
    assert_eq!(restored_generation, generation);
    assert!(Arc::ptr_eq(&restored, &old));

    let mut stale = image.prepare(generation + 1, String::from("stale"));
    assert_eq!(stale.publish(), Err(RuntimeExecError::Failed));
    assert!(Arc::ptr_eq(&image.current().1, &old));
}
