use super::{
    close::{CaptureEvent, CaptureOutcome},
    model::ProcessKey,
    *,
};
use crate::runtime::checkpoint::authority::PrepareId;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn id(value: u8) -> PrepareId {
    PrepareId([value; 16])
}

fn root() -> ProcessIdentity {
    ProcessIdentity {
        key: ProcessKey::new(1, 1).unwrap(),
        parent: None,
    }
}

fn child(number: i32, parent: ProcessIdentity) -> ProcessIdentity {
    ProcessIdentity {
        key: ProcessKey::new(number, number as u64).unwrap(),
        parent: Some(parent.key),
    }
}

fn saved(identity: ProcessIdentity) -> SavedProcessIdentity {
    SavedProcessIdentity {
        key: identity.key,
        parent: identity.parent,
        member: MemberOrdinal::new(identity.key.pid as u64).unwrap(),
    }
}

fn namespace(members: impl IntoIterator<Item = SavedProcessIdentity>) -> OfdNamespace {
    let members = members.into_iter().collect::<Vec<_>>();
    OfdNamespace {
        lineage: LineageId::new(id(84)).unwrap(),
        generation: CheckpointGeneration::new(id(83)).unwrap(),
        next_member: members
            .iter()
            .map(|identity| identity.member)
            .max()
            .unwrap()
            .next()
            .unwrap(),
        next: members
            .into_iter()
            .map(|identity| (identity.member, std::num::NonZeroU64::MIN))
            .collect(),
    }
}

fn authority() -> Authority<u64> {
    Authority::new(
        Epoch::new(1).unwrap(),
        CloseId::new(id(1)).unwrap(),
        root(),
        Publication::new(id(80), id(81), 44).unwrap(),
        LifecycleRole::new(id(82)).unwrap(),
        LineageId::new(id(84)).unwrap(),
        CheckpointGeneration::new(id(83)).unwrap(),
    )
    .unwrap()
}

fn complete_edge(authority: &Authority<u64>, admission: &ForkAdmission, process: ProcessIdentity) {
    authority.process_started(admission.child, process).unwrap();
    authority.parent_report(admission.event, process).unwrap();
    authority.child_report(admission.child, process).unwrap();
    authority.child_ready(admission.child).unwrap();
    authority
        .published(admission.event, Publication::new(id(90), id(91), 7).unwrap())
        .unwrap();
    authority.release(admission.event).unwrap();
    authority.consume(admission.child).unwrap();
    authority
        .terminal(ticket::TerminalEvent {
            admission: admission.event,
            task: id(90),
            resource: id(91),
            lifecycle: LifecycleRole::new(id(92)).unwrap(),
        })
        .unwrap();
}

struct Plans;
impl ReservationPlanner for Plans {
    type Plan = ReservationKey;
    fn address_space(&mut self, saved: SavedProcessIdentity) -> Result<AddressSpaceOrdinal, AdmissionError> {
        AddressSpaceOrdinal::new(saved.member.get())
    }
    fn plan(&mut self, key: ReservationKey, _: &[SavedProcessIdentity]) -> Result<Self::Plan, AdmissionError> {
        Ok(key)
    }
}

struct SharedPlans;
impl ReservationPlanner for SharedPlans {
    type Plan = usize;
    fn address_space(&mut self, _: SavedProcessIdentity) -> Result<AddressSpaceOrdinal, AdmissionError> {
        AddressSpaceOrdinal::new(1)
    }
    fn plan(&mut self, _: ReservationKey, members: &[SavedProcessIdentity]) -> Result<Self::Plan, AdmissionError> {
        Ok(members.len())
    }
}

fn prepare(restore: &RestoreAdmission<'_, u64>) {
    let prepared = restore.prepare_reservations(&mut Plans).unwrap();
    assert_eq!(prepared.lineage, LineageId::new(id(84)).unwrap());
    assert_eq!(prepared.generation, CheckpointGeneration::new(id(83)).unwrap());
    assert!(!prepared.plans.is_empty());
    assert_eq!(prepared.init_member, MemberOrdinal::new(1).unwrap());
    assert_eq!(prepared.source.len(), prepared.plans.len());
}

#[test]
fn ticket_authority_is_sparse_and_has_no_fixed_capacity() {
    let authority = authority();
    let parent = root();
    for offset in 0..300_u16 {
        let process = child(i32::from(offset) + 2, parent);
        let mut ticket = [0_u8; 16];
        ticket[..2].copy_from_slice(&offset.to_le_bytes());
        ticket[15] = 1;
        let admission = authority
            .reserve_fork(
                TicketId::new(PrepareId(ticket)).unwrap(),
                ParentRole::new(id(2)).unwrap(),
                ChildRole::new(id(3)).unwrap(),
                parent,
            )
            .unwrap();
        complete_edge(&authority, &admission, process);
    }
    assert_eq!(authority.lock().unwrap().members.len(), 301);
}

#[test]
fn release_before_both_reports_ready_and_publication_is_rejected() {
    let authority = authority();
    let admission = authority
        .reserve_fork(
            TicketId::new(id(4)).unwrap(),
            ParentRole::new(id(5)).unwrap(),
            ChildRole::new(id(6)).unwrap(),
            root(),
        )
        .unwrap();
    assert_eq!(authority.release(admission.event), Err(AdmissionError::Unauthorized));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
    assert_eq!(
        authority.process_started(admission.child, child(2, root())),
        Err(AdmissionError::Poisoned)
    );
}

#[test]
fn stale_epoch_close_ticket_and_crossed_role_are_rejected() {
    let authority = authority();
    let admission = authority
        .reserve_fork(
            TicketId::new(id(7)).unwrap(),
            ParentRole::new(id(8)).unwrap(),
            ChildRole::new(id(9)).unwrap(),
            root(),
        )
        .unwrap();
    let mut stale = admission.event;
    stale.epoch = Epoch::new(2).unwrap();
    assert_eq!(
        authority.parent_report(stale, child(2, root())),
        Err(AdmissionError::Stale)
    );
    let mut wrong = admission.event;
    wrong.role = ParentRole::new(id(10)).unwrap();
    assert_eq!(
        authority.parent_report(wrong, child(2, root())),
        Err(AdmissionError::Unauthorized)
    );
}

#[test]
fn topology_rejects_pid_reuse_orphans_and_cycles() {
    let root = root();
    let first = child(2, root);
    let reused = ProcessIdentity {
        key: ProcessKey::new(2, 99).unwrap(),
        parent: Some(root.key),
    };
    assert_eq!(
        model::validate_topology(root, &HashSet::from([root, first, reused])),
        Err(AdmissionError::Conflict)
    );
    let orphan = ProcessIdentity {
        key: ProcessKey::new(3, 3).unwrap(),
        parent: Some(ProcessKey::new(99, 99).unwrap()),
    };
    assert_eq!(
        model::validate_topology(root, &HashSet::from([root, orphan])),
        Err(AdmissionError::Conflict)
    );
    let a_key = ProcessKey::new(4, 4).unwrap();
    let b_key = ProcessKey::new(5, 5).unwrap();
    let a = ProcessIdentity {
        key: a_key,
        parent: Some(b_key),
    };
    let b = ProcessIdentity {
        key: b_key,
        parent: Some(a_key),
    };
    assert_eq!(
        model::validate_topology(root, &HashSet::from([root, a, b])),
        Err(AdmissionError::Conflict)
    );
}

struct Storage(Arc<Mutex<Vec<&'static str>>>);
impl close::StorageGuard for Storage {
    fn commit(&mut self) -> super::super::authority::CommitOutcome {
        self.0.lock().unwrap().push("commit");
        super::super::authority::CommitOutcome::Published
    }
    fn reconcile(&mut self) -> super::super::authority::CommitOutcome {
        super::super::authority::CommitOutcome::Published
    }
    fn rollback(&mut self) -> Result<(), AdmissionError> {
        self.0.lock().unwrap().push("rollback");
        Ok(())
    }
}

struct AmbiguousStorage(Arc<Mutex<Vec<&'static str>>>);
impl close::StorageGuard for AmbiguousStorage {
    fn commit(&mut self) -> super::super::authority::CommitOutcome {
        self.0.lock().unwrap().push("unknown");
        super::super::authority::CommitOutcome::PublicationUnknown
    }
    fn reconcile(&mut self) -> super::super::authority::CommitOutcome {
        self.0.lock().unwrap().push("reconciled");
        super::super::authority::CommitOutcome::Published
    }
    fn rollback(&mut self) -> Result<(), AdmissionError> {
        panic!("ambiguous publication must never roll back")
    }
}

struct Freeze {
    snapshot: ResourceSnapshot<u64>,
    events: Arc<Mutex<Vec<&'static str>>>,
}
impl close::FreezeGuard<u64> for Freeze {
    fn snapshot(&self) -> &ResourceSnapshot<u64> {
        &self.snapshot
    }
    fn release(&mut self) -> Result<(), AdmissionError> {
        self.events.lock().unwrap().push("release");
        Ok(())
    }
}

#[test]
fn pre_publication_drop_releases_resources_and_rolls_back_storage() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut close = authority
        .begin_close::<Storage, Freeze>(CloseId::new(id(1)).unwrap(), Storage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), CaptureChannel::new(id(11)).unwrap())]),
            },
            events: events.clone(),
        })
        .unwrap();
    drop(close);
    assert_eq!(&*events.lock().unwrap(), &["release", "rollback"]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn eof_is_authenticated_exactly_once_and_poisoning_is_atomic() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let channel = CaptureChannel::new(id(12)).unwrap();
    let mut close = authority
        .begin_close::<Storage, Freeze>(CloseId::new(id(1)).unwrap(), Storage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), channel)]),
            },
            events,
        })
        .unwrap();
    close.publish().unwrap();
    let eof = CaptureEvent {
        epoch: Epoch::new(1).unwrap(),
        close: CloseId::new(id(1)).unwrap(),
        process: root(),
        channel,
        task: id(80),
        resource: id(81),
        outcome: CaptureOutcome::Eof,
    };
    assert_eq!(close.terminal(eof), Err(AdmissionError::Conflict));
    assert_eq!(close.terminal(eof), Err(AdmissionError::Stale));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
}

struct ExactReaper {
    calls: Arc<Mutex<Vec<HashSet<ProcessIdentity>>>>,
    fail: bool,
}
impl close::Reaper for ExactReaper {
    fn kill_and_reap(&mut self, exact: &HashSet<ProcessIdentity>) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        self.calls.lock().unwrap().push(exact.clone());
        (!self.fail).then(|| exact.clone()).ok_or(AdmissionError::Poisoned)
    }
}

struct RetryReaper {
    attempts: Arc<AtomicUsize>,
}
impl close::Reaper for RetryReaper {
    fn kill_and_reap(&mut self, exact: &HashSet<ProcessIdentity>) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(AdmissionError::Poisoned)
        } else {
            Ok(exact.clone())
        }
    }
}

#[test]
fn restore_owner_drop_physically_reaps_exact_live_set() {
    let authority = authority();
    let saved_root = root();
    let saved_child = child(2, saved_root);
    let live_root = ProcessIdentity {
        key: ProcessKey::new(101, 101).unwrap(),
        parent: None,
    };
    let live_child = child(102, live_root);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(saved_root), saved(saved_child)]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([
                    (saved(saved_root), CaptureChannel::new(id(30)).unwrap()),
                    (saved(saved_child), CaptureChannel::new(id(31)).unwrap()),
                ]),
            },
            namespace([saved(saved_root), saved(saved_child)]),
            ExactReaper {
                calls: calls.clone(),
                fail: false,
            },
        )
        .unwrap();
    assert!(matches!(
        restore.reserve(
            TicketId::new(id(69)).unwrap(),
            ParentRole::new(id(68)).unwrap(),
            ChildRole::new(id(67)).unwrap(),
            root(),
            saved(root()),
        ),
        Err(AdmissionError::Closed)
    ));
    prepare(&restore);
    let root_edge = restore
        .reserve(
            TicketId::new(id(40)).unwrap(),
            ParentRole::new(id(41)).unwrap(),
            ChildRole::new(id(42)).unwrap(),
            saved_root,
            saved(saved_root),
        )
        .unwrap();
    complete_edge(&authority, &root_edge, live_root);
    let child_edge = restore
        .reserve(
            TicketId::new(id(43)).unwrap(),
            ParentRole::new(id(44)).unwrap(),
            ChildRole::new(id(45)).unwrap(),
            live_root,
            saved(saved_child),
        )
        .unwrap();
    complete_edge(&authority, &child_edge, live_child);
    drop(restore);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[HashSet::from([live_root, live_child])]
    );
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn failed_physical_reap_cannot_reopen_restore_authority() {
    let authority = authority();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(30)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper { calls, fail: true },
        )
        .unwrap();
    prepare(&restore);
    let root_edge = restore
        .reserve(
            TicketId::new(id(50)).unwrap(),
            ParentRole::new(id(51)).unwrap(),
            ChildRole::new(id(52)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    complete_edge(&authority, &root_edge, root());
    drop(restore);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
}

#[test]
fn successful_capture_releases_freeze_before_committing_storage() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let channel = CaptureChannel::new(id(60)).unwrap();
    let mut close = authority
        .begin_close::<Storage, Freeze>(CloseId::new(id(1)).unwrap(), Storage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), channel)]),
            },
            events: events.clone(),
        })
        .unwrap();
    close.publish().unwrap();
    close
        .terminal(CaptureEvent {
            epoch: Epoch::new(1).unwrap(),
            close: CloseId::new(id(1)).unwrap(),
            process: root(),
            channel,
            task: id(80),
            resource: id(81),
            outcome: CaptureOutcome::Commit,
        })
        .unwrap();
    close.commit().unwrap();
    assert_eq!(&*events.lock().unwrap(), &["release", "commit"]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Committed);
}

#[test]
fn post_publication_owner_drop_rolls_back_but_never_reopens() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut close = authority
        .begin_close::<Storage, Freeze>(CloseId::new(id(1)).unwrap(), Storage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), CaptureChannel::new(id(61)).unwrap())]),
            },
            events: events.clone(),
        })
        .unwrap();
    close.publish().unwrap();
    drop(close);
    assert_eq!(&*events.lock().unwrap(), &["release", "rollback"]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
}

#[test]
fn capture_terminal_rejects_wrong_task_resource_binding() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let channel = CaptureChannel::new(id(62)).unwrap();
    let mut close = authority
        .begin_close::<Storage, Freeze>(CloseId::new(id(1)).unwrap(), Storage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), channel)]),
            },
            events,
        })
        .unwrap();
    close.publish().unwrap();
    assert_eq!(
        close.terminal(CaptureEvent {
            epoch: Epoch::new(1).unwrap(),
            close: CloseId::new(id(1)).unwrap(),
            process: root(),
            channel,
            task: id(99),
            resource: id(81),
            outcome: CaptureOutcome::Commit,
        }),
        Err(AdmissionError::Unauthorized)
    );
}

#[test]
fn restore_reservation_is_one_shot_and_digest_bound() {
    let authority = authority();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 70,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(71)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper { calls, fail: false },
        )
        .unwrap();
    prepare(&restore);
    let edge = restore
        .reserve(
            TicketId::new(id(72)).unwrap(),
            ParentRole::new(id(73)).unwrap(),
            ChildRole::new(id(74)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    assert!(matches!(
        restore.reserve(
            TicketId::new(id(75)).unwrap(),
            ParentRole::new(id(76)).unwrap(),
            ChildRole::new(id(77)).unwrap(),
            root(),
            saved(root()),
        ),
        Err(AdmissionError::Closed)
    ));
    authority.process_started(edge.child, root()).unwrap();
    authority.parent_report(edge.event, root()).unwrap();
    authority.child_report(edge.child, root()).unwrap();
    authority.child_ready(edge.child).unwrap();
    assert_eq!(
        authority.published(edge.event, Publication::new(id(90), id(91), 71).unwrap()),
        Err(AdmissionError::Conflict)
    );
}

#[test]
fn authenticated_role_loss_uses_owned_reaper_for_reported_process() {
    let authority = authority();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(78)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper {
                calls: calls.clone(),
                fail: false,
            },
        )
        .unwrap();
    prepare(&restore);
    let edge = restore
        .reserve(
            TicketId::new(id(79)).unwrap(),
            ParentRole::new(id(82)).unwrap(),
            ChildRole::new(id(83)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    authority.process_started(edge.child, root()).unwrap();
    authority.parent_report(edge.event, root()).unwrap();
    let reaped = restore.parent_lost(edge.event).unwrap();
    assert_eq!(reaped, HashSet::from([root()]));
    assert_eq!(calls.lock().unwrap().as_slice(), &[HashSet::from([root()])]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn restore_owner_drop_reaps_inflight_report_before_terminal() {
    let authority = authority();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(84)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper {
                calls: calls.clone(),
                fail: false,
            },
        )
        .unwrap();
    prepare(&restore);
    let edge = restore
        .reserve(
            TicketId::new(id(85)).unwrap(),
            ParentRole::new(id(86)).unwrap(),
            ChildRole::new(id(87)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    authority.process_started(edge.child, root()).unwrap();
    authority.parent_report(edge.event, root()).unwrap();
    authority.child_report(edge.child, root()).unwrap();
    drop(restore);
    assert_eq!(calls.lock().unwrap().as_slice(), &[HashSet::from([root()])]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn wrong_restore_role_cannot_authorize_owner_loss() {
    let authority = authority();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(88)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper {
                calls: calls.clone(),
                fail: false,
            },
        )
        .unwrap();
    prepare(&restore);
    let edge = restore
        .reserve(
            TicketId::new(id(89)).unwrap(),
            ParentRole::new(id(92)).unwrap(),
            ChildRole::new(id(93)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    authority.process_started(edge.child, root()).unwrap();
    authority.parent_report(edge.event, root()).unwrap();
    let mut forged = edge.event;
    forged.role = ParentRole::new(id(94)).unwrap();
    assert_eq!(restore.parent_lost(forged), Err(AdmissionError::Unauthorized));
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Restoring);
}

#[test]
fn second_restore_cycle_rejects_every_first_epoch_message() {
    let authority = authority();
    let snapshot = || ResourceSnapshot {
        digest: 7,
        channels: HashMap::from([(saved(root()), CaptureChannel::new(id(95)).unwrap())]),
    };
    let first = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            snapshot(),
            namespace([saved(root())]),
            ExactReaper {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            },
        )
        .unwrap();
    prepare(&first);
    let first_edge = first
        .reserve(
            TicketId::new(id(96)).unwrap(),
            ParentRole::new(id(97)).unwrap(),
            ChildRole::new(id(98)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    complete_edge(&authority, &first_edge, root());
    first.commit().unwrap();

    let second = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            snapshot(),
            namespace([saved(root())]),
            ExactReaper {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            },
        )
        .unwrap();
    prepare(&second);
    let second_edge = second
        .reserve(
            TicketId::new(id(99)).unwrap(),
            ParentRole::new(id(100)).unwrap(),
            ChildRole::new(id(101)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    assert_eq!(
        authority.parent_report(first_edge.event, root()),
        Err(AdmissionError::Stale)
    );
    complete_edge(&authority, &second_edge, root());
    second.commit().unwrap();
    assert_eq!(authority.lock().unwrap().epoch, Epoch::new(3).unwrap());
}

#[test]
fn restore_cleanup_failure_keeps_durable_retry_owner() {
    let authority = authority();
    let attempts = Arc::new(AtomicUsize::new(0));
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(102)).unwrap())]),
            },
            namespace([saved(root())]),
            RetryReaper {
                attempts: attempts.clone(),
            },
        )
        .unwrap();
    prepare(&restore);
    let edge = restore
        .reserve(
            TicketId::new(id(103)).unwrap(),
            ParentRole::new(id(104)).unwrap(),
            ChildRole::new(id(105)).unwrap(),
            root(),
            saved(root()),
        )
        .unwrap();
    authority.process_started(edge.child, root()).unwrap();
    drop(restore);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
    assert_eq!(authority.retry_cleanup().unwrap(), HashSet::from([root()]));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn restore_rejects_preexisting_normal_ticket() {
    let authority = authority();
    authority
        .reserve_fork(
            TicketId::new(id(106)).unwrap(),
            ParentRole::new(id(107)).unwrap(),
            ChildRole::new(id(108)).unwrap(),
            root(),
        )
        .unwrap();
    assert!(matches!(
        authority.begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root())]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([(saved(root()), CaptureChannel::new(id(109)).unwrap())]),
            },
            namespace([saved(root())]),
            ExactReaper {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            },
        ),
        Err(AdmissionError::Unauthorized)
    ));
}

#[test]
fn normal_fork_cancel_and_lifecycle_are_authenticated() {
    let authority = authority();
    let child = child(2, root());
    let admission = authority
        .reserve_fork(
            TicketId::new(id(110)).unwrap(),
            ParentRole::new(id(111)).unwrap(),
            ChildRole::new(id(112)).unwrap(),
            root(),
        )
        .unwrap();
    authority.process_started(admission.child, child).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut reaper = ExactReaper {
        calls: calls.clone(),
        fail: false,
    };
    assert_eq!(
        authority.cancel_fork(admission.child, &mut reaper).unwrap(),
        HashSet::from([child])
    );
    assert_eq!(calls.lock().unwrap().as_slice(), &[HashSet::from([child])]);

    let admitted = authority
        .reserve_fork(
            TicketId::new(id(113)).unwrap(),
            ParentRole::new(id(114)).unwrap(),
            ChildRole::new(id(115)).unwrap(),
            root(),
        )
        .unwrap();
    complete_edge(&authority, &admitted, child);
    let lifecycle = ticket::LifecycleEvent {
        epoch: Epoch::new(1).unwrap(),
        close: CloseId::new(id(1)).unwrap(),
        process: child,
        role: LifecycleRole::new(id(92)).unwrap(),
    };
    authority.exec(lifecycle).unwrap();
    let first = authority.allocate_ofd(lifecycle).unwrap();
    let second = authority.allocate_ofd(lifecycle).unwrap();
    assert_eq!(first.generation, CheckpointGeneration::new(id(83)).unwrap());
    // The cancelled fork consumed ordinal 2. Stable identities are never
    // recycled, even when physical child ownership is reaped successfully.
    assert_eq!(first.member, MemberOrdinal::new(3).unwrap());
    assert_eq!(first.sequence, std::num::NonZeroU64::MIN);
    assert_eq!(second.sequence.get(), 2);
    assert_eq!(authority.ofd_namespace().unwrap().next[&first.member].get(), 3);
    authority.exit(lifecycle).unwrap();
    assert!(!authority.lock().unwrap().members.contains(&child));
}

#[test]
fn indeterminate_storage_publication_is_reconciled_without_rollback() {
    let authority = authority();
    let events = Arc::new(Mutex::new(Vec::new()));
    let channel = CaptureChannel::new(id(116)).unwrap();
    let mut close = authority
        .begin_close::<AmbiguousStorage, Freeze>(CloseId::new(id(1)).unwrap(), AmbiguousStorage(events.clone()))
        .unwrap();
    close.drain(Instant::now() + Duration::from_millis(10)).unwrap();
    close
        .freeze(Freeze {
            snapshot: ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(root(), channel)]),
            },
            events: events.clone(),
        })
        .unwrap();
    close.publish().unwrap();
    close
        .terminal(CaptureEvent {
            epoch: Epoch::new(1).unwrap(),
            close: CloseId::new(id(1)).unwrap(),
            process: root(),
            channel,
            task: id(80),
            resource: id(81),
            outcome: CaptureOutcome::Commit,
        })
        .unwrap();
    assert_eq!(close.commit(), Err(AdmissionError::Poisoned));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Poisoned);
    assert_eq!(
        authority.reconcile_storage().unwrap(),
        super::super::authority::CommitOutcome::Published
    );
    assert_eq!(&*events.lock().unwrap(), &["release", "unknown", "reconciled"]);
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Committed);
}

#[test]
fn shared_address_space_is_planned_once_for_all_thread_members() {
    let authority = authority();
    let root = root();
    let thread = child(2, root);
    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved(root), saved(thread)]),
            ResourceSnapshot {
                digest: 7,
                channels: HashMap::from([
                    (saved(root), CaptureChannel::new(id(117)).unwrap()),
                    (saved(thread), CaptureChannel::new(id(118)).unwrap()),
                ]),
            },
            namespace([saved(root), saved(thread)]),
            ExactReaper {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            },
        )
        .unwrap();
    let domain = restore.prepare_reservations(&mut SharedPlans).unwrap();
    assert_eq!(domain.plans.len(), 1);
    assert_eq!(domain.plans[&AddressSpaceOrdinal::new(1).unwrap()], 2);
    assert_eq!(domain.source.len(), 2);
    assert!(
        domain
            .source
            .values()
            .all(|binding| binding.address_space == AddressSpaceOrdinal::new(1).unwrap())
    );
}

#[test]
fn restore_rejects_a_different_checkpoint_lineage() {
    let authority = authority();
    let root = saved(root());
    let mut ofd = namespace([root]);
    ofd.lineage = LineageId::new(id(85)).unwrap();

    let result = authority.begin_restore(
        CloseId::new(id(1)).unwrap(),
        HashSet::from([root]),
        ResourceSnapshot {
            digest: 44,
            channels: HashMap::from([(root, CaptureChannel::new(id(86)).unwrap())]),
        },
        ofd,
        ExactReaper {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        },
    );

    assert_eq!(result.err(), Some(AdmissionError::Stale));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn restore_rejects_member_counter_rollback() {
    let authority = authority();
    let live_root = root();
    let live_child = child(2, live_root);
    let admission = authority
        .reserve_fork(
            TicketId::new(id(120)).unwrap(),
            ParentRole::new(id(121)).unwrap(),
            ChildRole::new(id(122)).unwrap(),
            live_root,
        )
        .unwrap();
    complete_edge(&authority, &admission, live_child);
    let saved_root = saved(live_root);
    let saved_child = saved(live_child);
    let mut ofd = namespace([saved_root, saved_child]);
    ofd.next_member = MemberOrdinal::new(2).unwrap();

    let result = authority.begin_restore(
        CloseId::new(id(1)).unwrap(),
        HashSet::from([saved_root, saved_child]),
        ResourceSnapshot {
            digest: 44,
            channels: HashMap::from([
                (saved_root, CaptureChannel::new(id(123)).unwrap()),
                (saved_child, CaptureChannel::new(id(124)).unwrap()),
            ]),
        },
        ofd,
        ExactReaper {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        },
    );

    assert_eq!(result.err(), Some(AdmissionError::Stale));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn restore_rejects_ofd_counter_rollback() {
    let authority = authority();
    let live_root = root();
    let lifecycle = ticket::LifecycleEvent {
        epoch: Epoch::new(1).unwrap(),
        close: CloseId::new(id(1)).unwrap(),
        process: live_root,
        role: LifecycleRole::new(id(82)).unwrap(),
    };
    assert_eq!(authority.allocate_ofd(lifecycle).unwrap().sequence.get(), 1);
    let saved_root = saved(live_root);

    let result = authority.begin_restore(
        CloseId::new(id(1)).unwrap(),
        HashSet::from([saved_root]),
        ResourceSnapshot {
            digest: 44,
            channels: HashMap::from([(saved_root, CaptureChannel::new(id(125)).unwrap())]),
        },
        namespace([saved_root]),
        ExactReaper {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        },
    );

    assert_eq!(result.err(), Some(AdmissionError::Stale));
    assert_eq!(authority.lock().unwrap().phase, ticket::Phase::Open);
}

#[test]
fn restore_adopts_authenticated_monotonic_counters() {
    let authority = authority();
    let saved_root = saved(root());
    let mut ofd = namespace([saved_root]);
    ofd.next_member = MemberOrdinal::new(9).unwrap();
    ofd.next
        .insert(saved_root.member, std::num::NonZeroU64::new(7).unwrap());

    let restore = authority
        .begin_restore(
            CloseId::new(id(1)).unwrap(),
            HashSet::from([saved_root]),
            ResourceSnapshot {
                digest: 44,
                channels: HashMap::from([(saved_root, CaptureChannel::new(id(126)).unwrap())]),
            },
            ofd,
            ExactReaper {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            },
        )
        .unwrap();

    let state = authority.lock().unwrap();
    assert_eq!(state.next_member, MemberOrdinal::new(9).unwrap());
    assert_eq!(state.ofd_next[&saved_root.member].get(), 7);
    drop(state);
    drop(restore);
}
