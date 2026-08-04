use std::sync::Arc;

use hl_event::{Epoll, EventCatalog, EventFd, EventFdFlags};
use hl_task::{ForkCloneFlags, ForkEntityId, ForkRequest};

use crate::{
    EventForkParticipant, ForkCancellation, ForkContext, ForkCoordinator, ForkParticipant, ForkParticipantRole,
};

struct NeverCancel;

impl ForkCancellation for NeverCancel {
    fn cancelled(&self) -> bool {
        false
    }
}

struct Passive {
    role: ForkParticipantRole,
    fail_commit: bool,
}

impl ForkParticipant for Passive {
    fn role(&self) -> ForkParticipantRole {
        self.role
    }
    fn prepare(&self, _: ForkContext) -> Result<u64, ()> {
        Ok(self.role as u64 + 1)
    }
    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn clone_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn commit(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        if self.fail_commit { Err(()) } else { Ok(()) }
    }
    fn rollback(&self, _: ForkContext, _: u64) {}
}

struct Fixture;

impl Fixture {
    fn request() -> ForkRequest {
        ForkRequest {
            parent: ForkEntityId { slot: 1, generation: 1 },
            child: ForkEntityId { slot: 2, generation: 1 },
            flags: ForkCloneFlags::default(),
        }
    }

    fn participants(event: Arc<EventForkParticipant>, execution_commit_fails: bool) -> Vec<Arc<dyn ForkParticipant>> {
        [
            ForkParticipantRole::Task,
            ForkParticipantRole::Descriptors,
            ForkParticipantRole::Memory,
            ForkParticipantRole::Provider,
            ForkParticipantRole::Execution,
            ForkParticipantRole::Network,
        ]
        .into_iter()
        .map(|role| {
            Arc::new(Passive {
                role,
                fail_commit: execution_commit_fails && role == ForkParticipantRole::Execution,
            }) as Arc<dyn ForkParticipant>
        })
        .chain(std::iter::once(event as Arc<dyn ForkParticipant>))
        .chain(std::iter::once(Arc::new(Passive {
            role: ForkParticipantRole::Ipc,
            fail_commit: false,
        }) as Arc<dyn ForkParticipant>))
        .collect()
    }
}

#[test]
fn child_subscription_object() {
    let parent = Arc::new(EventCatalog::new(4).unwrap());
    let eventfd = Arc::new(EventFd::new(3, EventFdFlags::default()).unwrap());
    let eventfd_id = parent.insert_eventfd(Arc::clone(&eventfd)).unwrap();
    let epoll = Arc::new(Epoll::new());
    let epoll_id = parent.insert_epoll(Arc::clone(&epoll), Vec::new()).unwrap();
    let participant = Arc::new(EventForkParticipant::new(Arc::clone(&parent)));
    let outcome = ForkCoordinator::new(Fixture::participants(Arc::clone(&participant), false))
        .unwrap()
        .fork(Fixture::request(), &NeverCancel)
        .unwrap();
    let child = participant.take_child(outcome.context.transaction).unwrap();

    assert!(Arc::ptr_eq(&parent, &child));
    child
        .with_eventfd(eventfd_id, |object| {
            object.write(&5_u64.to_ne_bytes()).unwrap();
        })
        .unwrap();
    assert_eq!(parent.with_eventfd(eventfd_id, EventFd::counter), Ok(8));
    assert_eq!(
        child.with_epoll(epoll_id, |object| std::ptr::eq(object, epoll.as_ref())),
        Ok(true),
    );
}

#[test]
fn downstream_membership_exactly() {
    let parent = Arc::new(EventCatalog::new(2).unwrap());
    let object = Arc::new(EventFd::new(9, EventFdFlags::default()).unwrap());
    let id = parent.insert_eventfd(object).unwrap();
    let participant = Arc::new(EventForkParticipant::new(Arc::clone(&parent)));
    let references = Arc::strong_count(&parent);

    let result = ForkCoordinator::new(Fixture::participants(Arc::clone(&participant), true))
        .unwrap()
        .fork(Fixture::request(), &NeverCancel);

    assert!(result.is_err());
    assert_eq!(participant.staged_count(), 0);
    assert!(participant.child(1).is_none());
    assert_eq!(Arc::strong_count(&parent), references);
    assert_eq!(parent.with_eventfd(id, EventFd::counter), Ok(9));
}
