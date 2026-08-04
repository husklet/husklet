use std::sync::Arc;

use hl_network::{NetworkCatalog, NetworkConfiguration};
use hl_task::{ForkCloneFlags, ForkEntityId, ForkRequest};

use crate::{
    ForkCancellation, ForkContext, ForkCoordinator, ForkParticipant, ForkParticipantRole, NetworkForkParticipant,
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

fn request() -> ForkRequest {
    ForkRequest {
        parent: ForkEntityId { slot: 1, generation: 1 },
        child: ForkEntityId { slot: 2, generation: 1 },
        flags: ForkCloneFlags::default(),
    }
}

fn catalog() -> Arc<NetworkCatalog> {
    Arc::new(NetworkCatalog::new(
        NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
    ))
}

struct Fixture;

impl Fixture {
    fn participants(
        network: Arc<NetworkForkParticipant>,
        execution_commit_fails: bool,
    ) -> Vec<Arc<dyn ForkParticipant>> {
        vec![
            Arc::new(Passive {
                role: ForkParticipantRole::Task,
                fail_commit: false,
            }),
            Arc::new(Passive {
                role: ForkParticipantRole::Descriptors,
                fail_commit: false,
            }),
            Arc::new(Passive {
                role: ForkParticipantRole::Memory,
                fail_commit: false,
            }),
            Arc::new(Passive {
                role: ForkParticipantRole::Provider,
                fail_commit: false,
            }),
            Arc::new(Passive {
                role: ForkParticipantRole::Execution,
                fail_commit: execution_commit_fails,
            }),
            network,
            Arc::new(Passive {
                role: ForkParticipantRole::Event,
                fail_commit: false,
            }),
            Arc::new(Passive {
                role: ForkParticipantRole::Ipc,
                fail_commit: false,
            }),
        ]
    }
}

#[test]
fn child_catalog_identity() {
    let parent = catalog();
    let network = Arc::new(NetworkForkParticipant::new(Arc::clone(&parent)));
    let outcome = ForkCoordinator::new(Fixture::participants(Arc::clone(&network), false))
        .unwrap()
        .fork(request(), &NeverCancel)
        .unwrap();
    let child = network.take_child(outcome.context.transaction).unwrap();
    assert!(Arc::ptr_eq(&parent, &child));
}

#[test]
fn downstream_touching_catalog() {
    let parent = catalog();
    let network = Arc::new(NetworkForkParticipant::new(Arc::clone(&parent)));
    let references = Arc::strong_count(&parent);
    let error = ForkCoordinator::new(Fixture::participants(Arc::clone(&network), true))
        .unwrap()
        .fork(request(), &NeverCancel);
    assert!(error.is_err());
    assert_eq!(network.staged_count(), 0);
    assert!(network.child(1).is_none());
    assert_eq!(Arc::strong_count(&parent), references);
    parent.freeze_checkpoint();
    let image = parent.checkpoint_image().unwrap();
    parent.thaw_checkpoint();
    assert!(image.sockets.is_empty());
}
