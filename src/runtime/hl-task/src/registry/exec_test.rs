use std::sync::Arc;

use crate::{
    AlternateStack, ChildClass, PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig,
    RobustListRegistration, SignalAction, SignalDisposition, SignalMask, SignalNumber, TaskError, TaskRegistry,
};

struct Fixture {
    registry: Arc<TaskRegistry>,
    process: crate::ProcessId,
    thread: crate::ThreadId,
    handled: SignalNumber,
    ignored: SignalNumber,
    stack: AlternateStack,
    robust: RobustListRegistration,
}

impl Fixture {
    fn new() -> Self {
        let registry = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(10, 20, &[], 8).unwrap();
        let (process, thread) = registry.create_init(credentials, ProcessLimits::empty()).unwrap();
        let handled = SignalNumber::new(10).unwrap();
        let ignored = SignalNumber::new(12).unwrap();
        registry
            .set_action(
                process,
                handled,
                SignalAction {
                    disposition: SignalDisposition::Handler(0x1000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        registry
            .set_action(
                process,
                ignored,
                SignalAction {
                    disposition: SignalDisposition::Ignore,
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        registry.set_signal_mask(thread, SignalMask::from_bits(3)).unwrap();
        let stack = AlternateStack::Enabled {
            pointer: 0x8000,
            size: 0x4000,
        };
        registry.set_alternate_stack(thread, stack).unwrap();
        let robust = RobustListRegistration::new(0x9000);
        registry.set_robust_list(thread, robust).unwrap();
        registry.set_clear_tid(thread, 0xa000).unwrap();
        Self {
            registry,
            process,
            thread,
            handled,
            ignored,
            stack,
            robust,
        }
    }
}

#[test]
fn prepared_exec_resets() {
    let fixture = Fixture::new();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap();
    prepared.publish().unwrap();
    let published = fixture.registry.exec_plan(fixture.thread).unwrap();
    assert_eq!(
        published.process.actions,
        [(
            fixture.ignored,
            SignalAction {
                disposition: SignalDisposition::Ignore,
                ..SignalAction::DEFAULT
            },
        )],
    );
    assert_eq!(published.thread.alternate_stack, AlternateStack::Disabled,);
    assert_eq!(fixture.registry.robust_list(fixture.thread).unwrap(), None);
    assert_eq!(fixture.registry.clear_tid(fixture.thread).unwrap(), None);

    prepared.rollback();
    let restored = fixture.registry.fork_plan(fixture.thread).unwrap();
    assert!(restored.process.actions.iter().any(|(signal, action)| {
        *signal == fixture.handled && action.disposition == SignalDisposition::Handler(0x1000)
    }));
    assert_eq!(restored.thread.alternate_stack, fixture.stack);
    assert_eq!(
        fixture.registry.robust_list(fixture.thread).unwrap(),
        Some(fixture.robust),
    );
    assert_eq!(fixture.registry.clear_tid(fixture.thread).unwrap(), Some(0xa000));
}

#[test]
fn image_arguments_publish_and_rollback_atomically() {
    let fixture = Fixture::new();
    fixture
        .registry
        .publish_arguments(fixture.process, vec![b"old".to_vec()])
        .unwrap();
    let mut prepared = fixture
        .registry
        .prepare_image(
            fixture.process,
            fixture.thread,
            *b"new\0\0\0\0\0\0\0\0\0\0\0\0\0",
            vec![b"new".to_vec(), b"argument".to_vec()],
        )
        .unwrap();
    prepared.publish().unwrap();
    assert_eq!(
        fixture.registry.snapshot().processes[0].arguments,
        [b"new".to_vec(), b"argument".to_vec()]
    );
    prepared.rollback();
    assert_eq!(fixture.registry.snapshot().processes[0].arguments, [b"old".to_vec()]);
}

#[test]
fn dropped_unpublished_exec() {
    let fixture = Fixture::new();
    drop(fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap());
    let mut retry = fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap();
    retry.publish().unwrap();
    retry.finish();
}

#[test]
fn exec_publish_retires() {
    let fixture = Fixture::new();
    let plan = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    let sibling = fixture.registry.commit_clone_thread(plan).unwrap();
    let before = fixture.registry.snapshot();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap();
    assert!(fixture.registry.begin_clone_thread(fixture.thread).is_err());
    assert!(
        fixture
            .registry
            .exit_thread(sibling, crate::ExitStatus::Code(0))
            .is_err()
    );
    prepared.publish().unwrap();
    let published = fixture.registry.snapshot();
    assert_eq!(published.processes[0].threads, vec![fixture.thread]);
    assert_eq!(published.threads.len(), 1);
    prepared.rollback();
    let restored = fixture.registry.snapshot();
    assert_eq!(restored.processes, before.processes);
    assert_eq!(restored.threads, before.threads);
    assert_eq!(restored.thread_generations, before.thread_generations);
}

#[test]
fn finished_exec_releases() {
    let fixture = Fixture::new();
    let plan = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    let sibling = fixture.registry.commit_clone_thread(plan).unwrap();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap();
    prepared.publish().unwrap();
    prepared.finish();
    assert!(fixture.registry.fork_plan(sibling).is_err());
    let replacement = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    assert_eq!(replacement.thread().number(), sibling.number());
    assert_ne!(replacement.thread(), sibling);
}

#[test]
fn prepared_exec_blocks() {
    let fixture = Fixture::new();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, fixture.thread).unwrap();
    let action = SignalAction {
        disposition: SignalDisposition::Handler(0x2000),
        ..SignalAction::DEFAULT
    };
    let registration = RobustListRegistration::new(0xa000);
    assert_eq!(
        fixture.registry.set_action(fixture.process, fixture.handled, action),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture
            .registry
            .set_signal_mask(fixture.thread, SignalMask::from_bits(7)),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture
            .registry
            .set_alternate_stack(fixture.thread, AlternateStack::Disabled),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture.registry.set_robust_list(fixture.thread, registration),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture.registry.request_cancellation(fixture.thread),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture.registry.set_child_class(fixture.process, ChildClass::Clone),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(
        fixture.registry.enqueue_signal(
            PendingTarget::Process(fixture.process),
            crate::SignalInfo::bare(fixture.handled),
        ),
        Err(TaskError::InvalidLifecycle),
    );

    prepared.rollback();
    fixture
        .registry
        .set_action(fixture.process, fixture.handled, action)
        .unwrap();
    fixture
        .registry
        .set_signal_mask(fixture.thread, SignalMask::from_bits(7))
        .unwrap();
    fixture
        .registry
        .set_alternate_stack(fixture.thread, AlternateStack::Disabled)
        .unwrap();
    fixture.registry.set_robust_list(fixture.thread, registration).unwrap();
    fixture.registry.request_cancellation(fixture.thread).unwrap();
    fixture
        .registry
        .set_child_class(fixture.process, ChildClass::Clone)
        .unwrap();
}

#[test]
fn nonleader_exec_assumes() {
    let fixture = Fixture::new();
    let plan = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    let caller = fixture.registry.commit_clone_thread(plan).unwrap();
    let before = fixture.registry.snapshot();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, caller).unwrap();
    prepared.publish().unwrap();
    let published = fixture.registry.snapshot();
    assert_eq!(published.processes[0].leader, fixture.thread);
    assert_eq!(published.processes[0].threads, vec![fixture.thread]);
    assert_eq!(published.threads[0].id, fixture.thread);
    assert!(fixture.registry.fork_plan(caller).is_err());

    prepared.rollback();
    let restored = fixture.registry.snapshot();
    assert_eq!(restored.processes, before.processes);
    assert_eq!(restored.threads, before.threads);
    assert_eq!(restored.thread_generations, before.thread_generations);
}

#[test]
fn finished_nonleader_exec() {
    let fixture = Fixture::new();
    let plan = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    let caller = fixture.registry.commit_clone_thread(plan).unwrap();
    let mut prepared = fixture.registry.prepare_exec(fixture.process, caller).unwrap();
    prepared.publish().unwrap();
    prepared.finish();

    assert!(fixture.registry.fork_plan(caller).is_err());
    assert!(fixture.registry.fork_plan(fixture.thread).is_ok());
    let replacement = fixture.registry.begin_clone_thread(fixture.thread).unwrap();
    assert_eq!(replacement.thread().number(), caller.number());
    assert_ne!(replacement.thread(), caller);
}
