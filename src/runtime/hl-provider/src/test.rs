use std::sync::Arc;
use std::thread;

use super::*;

fn remote(value: u64) -> RemoteId {
    RemoteId::new(value).expect("nonzero test id")
}

#[test]
fn clone_closes_remote() {
    let namespace = HandleNamespace::new(2).unwrap();
    let handle = namespace.open(remote(41), HandleKind::File).unwrap();
    assert_eq!(namespace.clone_handle(handle), Ok(handle));
    assert_eq!(namespace.close(handle), Ok(None));
    let close = namespace.close(handle).unwrap().unwrap();
    assert_eq!(close.remote(), remote(41));
    assert_eq!(namespace.close(handle), Err(NamespaceError::InvalidHandle));
}

#[test]
fn reuse_changes_generation() {
    let namespace = HandleNamespace::new(1).unwrap();
    let stale = namespace.open(remote(1), HandleKind::File).unwrap();
    namespace.close(stale).unwrap();
    let current = namespace.open(remote(2), HandleKind::File).unwrap();
    assert_ne!(stale, current);
    assert_eq!(
        namespace.resolve(stale, HandleKind::File),
        Err(NamespaceError::InvalidHandle)
    );
    assert_eq!(namespace.resolve(current, HandleKind::File), Ok(remote(2)));
}

#[test]
fn capacity_and_kind() {
    let namespace = HandleNamespace::new(1).unwrap();
    let handle = namespace.open(remote(7), HandleKind::Directory).unwrap();
    assert_eq!(namespace.open(remote(8), HandleKind::File), Err(NamespaceError::Full));
    assert_eq!(
        namespace.resolve(handle, HandleKind::File),
        Err(NamespaceError::WrongKind)
    );
}

#[test]
fn reservation_boundary_rolls() {
    let namespace = HandleNamespace::with_limits(NamespaceLimits::new(2).unwrap()).unwrap();
    let first = namespace.reserve(HandleKind::File).unwrap();
    let second = namespace.reserve(HandleKind::Event).unwrap();
    assert!(matches!(
        namespace.reserve(HandleKind::Counter),
        Err(NamespaceError::Full)
    ));
    assert_eq!(namespace.snapshot().live, 0);
    assert_eq!(namespace.begin_fork().err(), Some(NamespaceError::Busy));

    drop(first);
    let replacement = namespace.reserve(HandleKind::Counter).unwrap();
    let replacement_handle = replacement.publish(remote(30)).unwrap();
    let second_handle = second.publish(remote(20)).unwrap();
    assert_eq!(
        namespace.resolve(replacement_handle, HandleKind::Counter),
        Ok(remote(30))
    );
    assert_eq!(namespace.resolve(second_handle, HandleKind::Event), Ok(remote(20)));
}

#[test]
fn reservation_stress() {
    let namespace = HandleNamespace::new(1).unwrap();
    let first = namespace.reserve(HandleKind::File).unwrap().publish(remote(1)).unwrap();
    namespace.close(first).unwrap();
    for identity in 2..=4_098_u64 {
        let reservation = namespace.reserve(HandleKind::File).unwrap();
        if identity % 7 == 0 {
            drop(reservation);
            continue;
        }
        let current = reservation.publish(remote(identity)).unwrap();
        assert_eq!(
            namespace.resolve(first, HandleKind::File),
            Err(NamespaceError::InvalidHandle)
        );
        assert_eq!(namespace.resolve(current, HandleKind::File), Ok(remote(identity)));
        namespace.close(current).unwrap();
    }
    assert_eq!(namespace.snapshot().live, 0);
}

#[test]
fn sole_owner_can() {
    let source = HandleNamespace::new(1).unwrap();
    let destination = HandleNamespace::new(1).unwrap();
    let old = source.open(remote(9), HandleKind::Mapping).unwrap();
    let capability = source.transfer(old).unwrap();
    assert_eq!(
        source.resolve(old, HandleKind::Mapping),
        Err(NamespaceError::InvalidHandle)
    );
    let moved = destination.accept(capability).unwrap();
    let close = destination.close(moved).unwrap().unwrap();
    assert_eq!(close.remote(), remote(9));
}

#[test]
fn failed_accept_returns() {
    let source = HandleNamespace::new(1).unwrap();
    let full = HandleNamespace::new(1).unwrap();
    full.open(remote(1), HandleKind::File).unwrap();
    let handle = source.open(remote(2), HandleKind::Event).unwrap();
    let capability = source.transfer(handle).unwrap();
    let (error, capability) = full.accept(capability).unwrap_err();
    assert_eq!(error, NamespaceError::Full);
    assert_eq!(capability.close().remote(), remote(2));
}

#[test]
fn shared_resource_cannot() {
    let namespace = HandleNamespace::new(1).unwrap();
    let handle = namespace.open(remote(3), HandleKind::Counter).unwrap();
    namespace.clone_handle(handle).unwrap();
    assert!(matches!(
        namespace.transfer(handle),
        Err(NamespaceError::SharedTransfer)
    ));
}

#[test]
fn concurrent_clones_preserve() {
    let namespace = Arc::new(HandleNamespace::new(1).unwrap());
    let handle = namespace.open(remote(11), HandleKind::File).unwrap();
    let workers: Vec<_> = (0..64)
        .map(|_| {
            let namespace = Arc::clone(&namespace);
            thread::spawn(move || {
                namespace.clone_handle(handle).unwrap();
                assert_eq!(namespace.close(handle), Ok(None));
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(namespace.snapshot().references, 1);
    assert!(namespace.close(handle).unwrap().is_some());
}

struct Model {
    live: Vec<(Handle, RemoteId, u32)>,
}

impl Model {
    fn exercise(&mut self, namespace: &HandleNamespace, step: u64) {
        if step % 3 != 0 && self.live.len() < 4 {
            let identity = remote(step);
            let handle = namespace.open(identity, HandleKind::Subscription).unwrap();
            self.live.push((handle, identity, 1));
            return;
        }
        if self.live.is_empty() {
            return;
        }
        let index = step as usize % self.live.len();
        if step % 5 == 0 {
            namespace.clone_handle(self.live[index].0).unwrap();
            self.live[index].2 += 1;
            return;
        }
        let result = namespace.close(self.live[index].0).unwrap();
        self.live[index].2 -= 1;
        if self.live[index].2 != 0 {
            assert_eq!(result, None);
            return;
        }
        assert_eq!(result.unwrap().remote(), self.live[index].1);
        self.live.swap_remove(index);
    }

    fn assert_matches(&self, namespace: &HandleNamespace) {
        let snapshot = namespace.snapshot();
        assert_eq!(snapshot.live, self.live.len());
        assert_eq!(
            snapshot.references,
            self.live.iter().map(|entry| u64::from(entry.2)).sum()
        );
        assert!(snapshot.entries.iter().all(|entry| entry.generation != 0));
    }
}

#[test]
fn deterministic_model_matches() {
    let namespace = HandleNamespace::new(4).unwrap();
    let mut model = Model { live: Vec::new() };
    for step in 1..=2_000_u64 {
        model.exercise(&namespace, step);
        model.assert_matches(&namespace);
    }
}

#[test]
fn revoke_invalidates_all() {
    let namespace = HandleNamespace::new(2).unwrap();
    let first = namespace.open(remote(1), HandleKind::File).unwrap();
    let second = namespace.open(remote(2), HandleKind::Event).unwrap();
    namespace.clone_handle(first).unwrap();
    let closes = namespace.revoke();
    assert_eq!(closes.len(), 2);
    assert_eq!(
        namespace.resolve(first, HandleKind::File),
        Err(NamespaceError::InvalidHandle)
    );
    assert_eq!(
        namespace.resolve(second, HandleKind::Event),
        Err(NamespaceError::InvalidHandle)
    );
}

#[test]
fn forked_namespaces_close() {
    for child_first in [false, true] {
        let parent = HandleNamespace::new(1).unwrap();
        let handle = parent.open(remote(31), HandleKind::File).unwrap();
        let child = parent.begin_fork().unwrap().commit();
        assert_eq!(child.resolve(handle, HandleKind::File), Ok(remote(31)));
        let (first, second) = if child_first {
            (&child, &parent)
        } else {
            (&parent, &child)
        };
        assert_eq!(first.close(handle), Ok(None));
        assert_eq!(second.close(handle).unwrap().unwrap().remote(), remote(31));
    }
}

#[test]
fn partial_fork_failure() {
    let parent = HandleNamespace::new(2).unwrap();
    let first = parent.open(remote(1), HandleKind::File).unwrap();
    parent.open(remote(2), HandleKind::Event).unwrap();
    assert!(matches!(parent.begin_fork_bounded(1), Err(NamespaceError::ForkLimit)));
    let capability = parent.transfer(first).unwrap();
    assert_eq!(capability.close().remote(), remote(1));
}

#[test]
fn active_fork_prevents() {
    let parent = HandleNamespace::new(1).unwrap();
    let handle = parent.open(remote(9), HandleKind::Transfer).unwrap();
    let plan = parent.begin_fork().unwrap();
    assert!(matches!(parent.transfer(handle), Err(NamespaceError::SharedTransfer)));
    assert_eq!(
        plan.snapshot().entries[0].generation,
        parent.snapshot().entries[0].generation
    );
    plan.rollback();
    assert_eq!(parent.transfer(handle).unwrap().close().remote(), remote(9));
}

#[test]
fn concurrent_parent_child() {
    let parent = Arc::new(HandleNamespace::new(1).unwrap());
    let handle = parent.open(remote(44), HandleKind::Counter).unwrap();
    let child = Arc::new(parent.begin_fork().unwrap().commit());
    let closes: Vec<_> = [parent, child]
        .into_iter()
        .map(|namespace| thread::spawn(move || namespace.close(handle).unwrap()))
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(closes.iter().filter(|close| close.is_some()).count(), 1);
    assert_eq!(closes.into_iter().flatten().next().unwrap().remote(), remote(44));
}

#[test]
fn child_reuse_is() {
    let parent = HandleNamespace::new(1).unwrap();
    let inherited = parent.open(remote(5), HandleKind::Event).unwrap();
    let child = parent.begin_fork().unwrap().commit();
    assert_eq!(child.close(inherited), Ok(None));
    let replacement = child.open(remote(6), HandleKind::Event).unwrap();
    assert_ne!(replacement, inherited);
    assert_eq!(parent.resolve(inherited, HandleKind::Event), Ok(remote(5)));
    assert_eq!(
        child.resolve(inherited, HandleKind::Event),
        Err(NamespaceError::InvalidHandle)
    );
    assert_eq!(child.close(replacement).unwrap().unwrap().remote(), remote(6));
    assert_eq!(parent.close(inherited).unwrap().unwrap().remote(), remote(5));
}

#[test]
fn pointer_free_fork() {
    let parent = HandleNamespace::new(1).unwrap();
    let handle = parent.open(remote(81), HandleKind::Directory).unwrap();
    let plan = parent.begin_fork().unwrap();
    let snapshot = plan.snapshot();
    plan.rollback();
    let child = parent.rebind_fork(&snapshot).unwrap().commit();
    assert_eq!(child.snapshot(), snapshot);
    assert_eq!(child.close(handle), Ok(None));
    assert_eq!(parent.close(handle).unwrap().unwrap().remote(), remote(81));

    let parent = HandleNamespace::new(1).unwrap();
    let handle = parent.open(remote(82), HandleKind::File).unwrap();
    let mut corrupt = parent.begin_fork().unwrap().snapshot();
    corrupt.entries[0].references += 1;
    assert!(matches!(
        parent.rebind_fork(&corrupt),
        Err(NamespaceError::InvalidSnapshot)
    ));
    assert_eq!(parent.transfer(handle).unwrap().close().remote(), remote(82));
}
