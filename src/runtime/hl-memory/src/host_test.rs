use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::WriteReservation;
use hl_isa::{AddressRange, GuestAddress};

use crate::{
    AtomicOperation, AtomicOrder, AtomicValue, Backing, ExternalSpan, FileIdentity, MapRequest, MappingBatch,
    MappingCoordinator, MappingHost, MappingOperation, MemoryAccessHost, MemoryError, Placement, Protection,
    SharedBackingRef, SharedError, SharedLimits, SharedObjectStore, SharedSeal,
};

#[derive(Clone, Copy)]
struct FakeProjection {
    address: u64,
    coherent: bool,
}

impl crate::HostProjection for FakeProjection {
    fn storage_address(&self) -> u64 {
        self.address
    }

    fn shared_backing_is_coherent(&self) -> bool {
        self.coherent
    }
}

#[derive(Debug)]
struct FakeHost {
    state: Mutex<FakeState>,
    fail_at: usize,
    coherent_projection: bool,
}

#[derive(Debug, Default)]
struct FakeState {
    calls: usize,
    live: Vec<u64>,
    transcript: Vec<String>,
    writes: BTreeMap<u64, AddressRange>,
    bytes: BTreeMap<u64, u8>,
}

impl FakeHost {
    fn failing(fail_at: usize) -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            fail_at,
            coherent_projection: false,
        }
    }

    fn coherent() -> Self {
        Self {
            coherent_projection: true,
            ..Self::failing(usize::MAX)
        }
    }

    fn operation(&self, name: &str) -> Result<u64, MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let call = state.calls;
        state.calls += 1;
        state.transcript.push(name.into());
        if call == self.fail_at {
            return Err(MemoryError::InvariantViolation);
        }
        let reservation = call as u64 + 1;
        state.live.push(reservation);
        Ok(reservation)
    }
}

impl MappingHost for FakeHost {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        self.operation("map")
    }
    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        self.operation("unmap")
    }
    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        self.operation("protect")
    }
    fn stage_remap(&self, _: AddressRange, _: GuestAddress, _: MapRequest, _: bool) -> Result<u64, MemoryError> {
        self.operation("remap")
    }
    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let call = state.calls;
        state.calls += 1;
        state.transcript.push("commit".into());
        if call == self.fail_at {
            return Err(MemoryError::InvariantViolation);
        }
        state.live.clear();
        Ok(())
    }
    fn rollback(&self, reservation: u64) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.transcript.push(format!("rollback:{reservation}"));
        state.live.retain(|entry| *entry != reservation);
    }
}

#[test]
fn remap_stage_rollback() {
    for failure in [2, 3] {
        let coordinator = MappingCoordinator::new(FakeHost::failing(failure));
        coordinator.map(request()).unwrap();
        let before = coordinator.ledger().regions();
        let mut moved = request();
        moved.placement = Placement::Fixed(GuestAddress::new(0x3000));
        assert_eq!(
            coordinator.remap(range(), moved, false),
            Err(MemoryError::InvariantViolation),
        );
        assert_eq!(coordinator.ledger().regions(), before);
        let state = coordinator.host.state.lock().unwrap();
        assert!(state.live.is_empty());
        if failure == 3 {
            assert!(state.transcript.iter().any(|entry| entry.starts_with("rollback:")));
        }
    }
}

#[test]
fn executable_alias_evidence_survives_remap_and_fork_restore() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let initial = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 1)
        .unwrap()
        .executable_aliases(GuestAddress::new(0x1000))
        .unwrap();
    assert!(!initial.present);

    let mut executable = writable;
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    coordinator.remap(range(), executable, true).unwrap();
    let remapped = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 2)
        .unwrap()
        .executable_aliases(GuestAddress::new(0x1000))
        .unwrap();
    assert!(remapped.present && remapped.generation > initial.generation);

    let child = coordinator.fork_restore(FakeHost::failing(usize::MAX)).unwrap();
    let child_evidence = child
        .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 3)
        .unwrap()
        .executable_aliases(GuestAddress::new(0x1000))
        .unwrap();
    assert_eq!(child_evidence, remapped);
}

impl MemoryAccessHost for FakeHost {
    type Projection = FakeProjection;

    fn project(&self, range: AddressRange) -> Result<FakeProjection, MemoryError> {
        Ok(FakeProjection {
            address: 0x1000_0000 + range.start().get(),
            coherent: self.coherent_projection,
        })
    }
    fn project_aperture(&self) -> Result<Option<crate::HostAperture<FakeProjection>>, MemoryError> {
        let range = AddressRange::nonempty(GuestAddress::ZERO, 0x1_0000).unwrap();
        crate::HostAperture::new(
            range,
            FakeProjection {
                address: 0x1000_0000,
                coherent: self.coherent_projection,
            },
        )
        .map(Some)
    }
    fn read(&self, range: AddressRange, output: &mut [u8], _: Protection) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (offset, byte) in output.iter_mut().enumerate() {
            *byte = state
                .bytes
                .get(&(range.start().get() + offset as u64))
                .copied()
                .unwrap_or(0);
        }
        Ok(())
    }
    fn prepare_write(&self, range: AddressRange) -> Result<WriteReservation, MemoryError> {
        let reservation = self.operation("prepare-write")?;
        self.state.lock().unwrap().writes.insert(reservation, range);
        Ok(WriteReservation::new(reservation, range))
    }
    fn commit_write(&self, reservation: WriteReservation, input: &[u8]) -> Result<(), MemoryError> {
        let reservation = reservation.token;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.transcript.push(format!("commit-write:{reservation}"));
        state.live.retain(|entry| *entry != reservation);
        let range = state
            .writes
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        if range.length() != input.len() as u64 {
            return Err(MemoryError::InvariantViolation);
        }
        for (offset, byte) in input.iter().enumerate() {
            state.bytes.insert(range.start().get() + offset as u64, *byte);
        }
        Ok(())
    }
    fn commit_external_write(&self, reservation: WriteReservation, length: u64) -> Result<(), MemoryError> {
        let reservation = reservation.token;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.transcript.push(format!("commit-external:{reservation}:{length}"));
        state.live.retain(|entry| *entry != reservation);
        let range = state
            .writes
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        if length > range.length() {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }
    fn rollback_write(&self, reservation: WriteReservation) {
        let reservation = reservation.token;
        self.state.lock().unwrap().writes.remove(&reservation);
        self.rollback(reservation);
    }
}

#[test]
fn aperture_contract() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    coordinator.map(request()).unwrap();
    let lease = coordinator.project_aperture(9).unwrap().unwrap();
    assert_eq!(lease.generation().incarnation, 9);
    assert_eq!(lease.storage_address(GuestAddress::new(0x1234), 8), Some(0x1000_1234));
    assert_eq!(lease.storage_address(GuestAddress::new(0xffff), 2), None);
    // The aperture covers holes and therefore cannot imply access authority.
    assert!(
        coordinator
            .ledger()
            .resolve(GuestAddress::new(0x8000), Protection::READ)
            .is_none()
    );

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let transitioning = Arc::clone(&coordinator);
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx.send(transitioning.unmap(range())).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(lease);
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    worker.join().unwrap();
}

#[test]
fn projection_blocks_mapping_transitions_and_checks_access() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 16, Protection::READ, 7)
        .unwrap();
    let continuation = lease.request_continuation();
    assert!(continuation.is_current());
    assert_eq!(lease.storage_address(), 0x1000_1000);
    assert_eq!(
        lease.range(),
        AddressRange::nonempty(GuestAddress::new(0x1000), 16).unwrap()
    );
    assert_eq!(lease.generation().incarnation, 7);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let transitioning = Arc::clone(&coordinator);
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = transitioning.unmap(range());
        done_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while continuation.is_current() && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(!continuation.is_current());
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(lease);
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    worker.join().unwrap();
    assert!(
        coordinator
            .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 7)
            .is_err()
    );
    assert!(!continuation.is_current());
}

#[test]
fn saturated_epoch_denies() {
    use std::sync::atomic::Ordering;

    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    coordinator.mapping_requests.store(u64::MAX, Ordering::Release);
    let projection = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 1)
        .unwrap();
    let continuation = projection.request_continuation();
    assert!(!continuation.is_current());
    drop(projection);
    coordinator.protect(range(), Protection::READ).unwrap();
    assert!(!continuation.is_current());
}

#[test]
fn direct_authority_is_generation_qualified_and_blocks_transitions() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let lease = coordinator
        .project_direct(GuestAddress::new(0x1000), 16, Protection::READ, 17)
        .unwrap();
    assert_eq!(
        lease.range(),
        AddressRange::nonempty(GuestAddress::new(0x1000), 16).unwrap()
    );
    assert_eq!(lease.storage_address(), 0x1000_1000);
    assert_eq!(lease.protection(), Protection::READ);
    assert_eq!(lease.generation().incarnation, 17);
    let (done_tx, done_rx) = mpsc::channel();
    let transitioning = Arc::clone(&coordinator);
    let worker = thread::spawn(move || done_tx.send(transitioning.unmap(range())).unwrap());
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(lease);
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    worker.join().unwrap();
    assert!(matches!(
        coordinator.project_direct(GuestAddress::new(0x1000), 1, Protection::NONE, 18),
        Err(MemoryError::InvariantViolation),
    ));
}

#[test]
fn existing_projection_converts_to_narrow_read_authority_and_returns() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let projection = coordinator
        .project_contiguous(
            GuestAddress::new(0x1000),
            16,
            Protection::READ.union(Protection::WRITE),
            23,
        )
        .unwrap();
    assert_eq!(projection.protection(), Protection::READ.union(Protection::WRITE));
    assert_eq!(projection.authority(), Protection::READ.union(Protection::WRITE));
    let authority = projection.into_direct(Protection::READ).unwrap();
    assert_eq!(authority.protection(), Protection::READ);
    assert_eq!(authority.generation().incarnation, 23);
    let projection = authority.into_projection();
    assert_eq!(projection.protection(), Protection::READ.union(Protection::WRITE));
    assert_eq!(projection.authority(), Protection::READ.union(Protection::WRITE));
    assert!(matches!(
        projection.into_direct(Protection::EXECUTE),
        Err(MemoryError::InvariantViolation),
    ));
}

#[test]
fn read_projection_keeps_mapped_execute_separate_from_invocation_authority() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::EXECUTE);
    coordinator.map(mapped).unwrap();

    let projection = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 16, Protection::READ, 29)
        .unwrap();

    assert_eq!(projection.protection(), Protection::READ.union(Protection::EXECUTE));
    assert_eq!(projection.authority(), Protection::READ);
    assert!(!projection.allows(Protection::EXECUTE));
    assert_eq!(
        projection.write_publication(GuestAddress::new(0x1000)),
        crate::WritePublication::Exact
    );
}

#[test]
fn publication_remains_exact() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut mapped = request();
    mapped.backing = Backing::Anonymous {
        identity: 6,
        shared: false,
    };
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 16, Protection::WRITE, 1)
        .unwrap();
    let before = {
        let state = coordinator.host.state.lock().unwrap();
        (
            state.calls,
            state.live.clone(),
            state.transcript.clone(),
            state.writes.clone(),
        )
    };
    assert_eq!(
        lease.write_publication(GuestAddress::new(0x1000)),
        crate::WritePublication::Exact
    );
    let after = {
        let state = coordinator.host.state.lock().unwrap();
        (
            state.calls,
            state.live.clone(),
            state.transcript.clone(),
            state.writes.clone(),
        )
    };
    assert_eq!(after, before);
}

#[test]
fn additional_projection_retains_execute_identity_and_dirty_ownership() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    let mut executable = request();
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.protection = Protection::READ.union(Protection::WRITE).union(Protection::EXECUTE);
    coordinator.map(executable).unwrap();
    executable.placement = Placement::Fixed(GuestAddress::new(0x5000));
    coordinator.map(executable).unwrap();
    let first = AddressRange::nonempty(GuestAddress::new(0x3004), 4).unwrap();
    let second = AddressRange::nonempty(GuestAddress::new(0x5008), 8).unwrap();
    let first_token = coordinator.executable_token(first, 1);
    let second_token = coordinator.executable_token(second, 1);
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 16, Protection::READ, 1)
        .unwrap();

    let read = lease
        .project_additional(GuestAddress::new(0x3000), 16, Protection::READ)
        .unwrap();
    let write = lease
        .project_additional(GuestAddress::new(0x3000), 16, Protection::WRITE)
        .unwrap();
    let other = lease
        .project_additional(GuestAddress::new(0x5000), 16, Protection::WRITE)
        .unwrap();

    assert_eq!(read.protection, Protection::READ.union(Protection::EXECUTE));
    assert_eq!(write.protection, Protection::WRITE.union(Protection::EXECUTE));
    assert_eq!(other.protection, Protection::WRITE.union(Protection::EXECUTE));
    lease.publish_written_ranges(&[first, second]).unwrap();
    assert_ne!(coordinator.executable_token(first, 1), first_token);
    assert_ne!(coordinator.executable_token(second, 1), second_token);
}

#[test]
fn writable_projection_requires_publication_or_rolls_back() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::WRITE, 1)
        .unwrap();
    drop(lease);
    assert!(
        coordinator
            .host
            .state
            .lock()
            .unwrap()
            .transcript
            .iter()
            .any(|entry| entry.starts_with("rollback:"))
    );
    coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::WRITE, 1)
        .unwrap()
        .publish_written()
        .unwrap();
    assert!(
        coordinator
            .host
            .state
            .lock()
            .unwrap()
            .transcript
            .iter()
            .any(|entry| entry.starts_with("commit-external:"))
    );
}

#[test]
fn projection_reuse_cap() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    for page in 0..=crate::LIVE_PROJECTION_MAXIMUM {
        let mut mapped = request();
        mapped.placement = Placement::Fixed(GuestAddress::new(0x1000 + page as u64 * 0x1000));
        mapped.backing = Backing::Anonymous {
            identity: page as u64 + 1,
            shared: false,
        };
        if page == 0 {
            mapped.protection = Protection::READ.union(Protection::WRITE);
        }
        coordinator.map(mapped).unwrap();
    }
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 16, Protection::READ, 9)
        .unwrap();
    let first = lease
        .project_additional(GuestAddress::new(0x2000), 32, Protection::READ)
        .unwrap();
    assert_eq!(
        lease
            .project_additional(GuestAddress::new(0x2008), 8, Protection::READ)
            .unwrap(),
        first
    );
    assert_eq!(lease.projection_count(), 2);
    let writable_primary = lease
        .project_additional(GuestAddress::new(0x1000), 1, Protection::WRITE)
        .unwrap();
    assert_eq!(writable_primary.protection, Protection::WRITE);
    assert_eq!(lease.projection_count(), 3);
    assert_eq!(
        lease.project_additional(GuestAddress::new(0x2ff8), 16, Protection::READ),
        Err(MemoryError::NoAddressSpace)
    );
    for page in 2..crate::LIVE_PROJECTION_MAXIMUM - 1 {
        lease
            .project_additional(GuestAddress::new(0x1000 + page as u64 * 0x1000), 1, Protection::READ)
            .unwrap();
    }
    assert_eq!(lease.projection_count(), crate::LIVE_PROJECTION_MAXIMUM);
    assert_eq!(
        lease.project_additional(
            GuestAddress::new(0x1000 + (crate::LIVE_PROJECTION_MAXIMUM - 1) as u64 * 0x1000),
            1,
            Protection::READ,
        ),
        Err(MemoryError::ResourceLimit)
    );
}

#[test]
fn projection_bounded_span() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    let mut mapped = request();
    mapped.placement = Placement::Fixed(GuestAddress::new(0x3000));
    mapped.backing = Backing::Anonymous {
        identity: 72,
        shared: false,
    };
    coordinator.map(mapped).unwrap();
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    let view = lease
        .project_bounded(GuestAddress::new(0x3180), 8, Protection::READ, 256)
        .unwrap();
    assert_eq!(view.range.start(), GuestAddress::new(0x3100));
    assert_eq!(view.range.end(), GuestAddress::new(0x3200));
    assert_eq!(
        lease.project_bounded(GuestAddress::new(0x3180), 257, Protection::READ, 256),
        Err(MemoryError::ResourceLimit)
    );
}

/// The native operand resolver passes an unbounded span so one view covers a
/// whole mapping; the clamp must still stop at the resolved region.
#[test]
fn projection_unbounded_span_stops_at_region() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    let mut mapped = request();
    mapped.placement = Placement::Fixed(GuestAddress::new(0x3000));
    mapped.length = 0x4000;
    mapped.backing = Backing::Anonymous {
        identity: 72,
        shared: false,
    };
    coordinator.map(mapped).unwrap();
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    let view = lease
        .project_bounded(GuestAddress::new(0x5180), 8, Protection::READ, u64::MAX)
        .unwrap();
    assert_eq!(view.range.start(), GuestAddress::new(0x3000));
    assert_eq!(view.range.end(), GuestAddress::new(0x7000));
}

/// The native run keeps four permission-keyed views, so a read-only projection
/// of a writable region costs a second slot that a read/write one does not.
#[test]
fn projection_read_write_span_serves_both_accesses_from_one_slot() {
    let build = || {
        let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
        coordinator.map(request()).unwrap();
        let mut mapped = request();
        mapped.placement = Placement::Fixed(GuestAddress::new(0x3000));
        mapped.protection = Protection::READ.union(Protection::WRITE);
        mapped.backing = Backing::Anonymous {
            identity: 72,
            shared: false,
        };
        coordinator.map(mapped).unwrap();
        coordinator
    };
    let read_write = Protection::READ.union(Protection::WRITE);

    let narrow = build();
    let mut lease = narrow
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    let read = lease
        .project_bounded(GuestAddress::new(0x3180), 8, Protection::READ, u64::MAX)
        .unwrap();
    let write = lease
        .project_bounded(GuestAddress::new(0x3180), 8, Protection::WRITE, u64::MAX)
        .unwrap();
    assert!(!read.protection.contains(Protection::WRITE));
    assert_ne!(read.index, write.index);

    let wide = build();
    let mut lease = wide
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    let first = lease
        .project_bounded(GuestAddress::new(0x3180), 8, read_write, u64::MAX)
        .unwrap();
    let second = lease
        .project_bounded(GuestAddress::new(0x3180), 8, Protection::READ, u64::MAX)
        .unwrap();
    assert!(first.protection.contains(read_write));
    assert_eq!(first.index, second.index);
}

#[test]
fn projection_rollback_epoch() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut primary = request();
    primary.protection = Protection::READ;
    coordinator.map(primary).unwrap();
    let mut executable = request();
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.protection = Protection::READ.union(Protection::WRITE).union(Protection::EXECUTE);
    executable.backing = Backing::Anonymous {
        identity: 33,
        shared: false,
    };
    coordinator.map(executable).unwrap();
    executable.placement = Placement::Fixed(GuestAddress::new(0x4000));
    executable.backing = Backing::Anonymous {
        identity: 34,
        shared: false,
    };
    coordinator.map(executable).unwrap();
    let source = AddressRange::nonempty(GuestAddress::new(0x3000), 8).unwrap();
    let rollback_token = coordinator.executable_token(source, 1);
    {
        let mut lease = coordinator
            .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
            .unwrap();
        lease
            .project_additional(GuestAddress::new(0x3000), 8, Protection::WRITE)
            .unwrap();
    }
    assert!(coordinator.host.state.lock().unwrap().live.is_empty());
    assert_eq!(coordinator.executable_token(source, 1), rollback_token);
    let before = coordinator.instruction_epoch();
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x3000), 8, Protection::WRITE)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x4000), 8, Protection::WRITE)
        .unwrap();
    lease.publish_written().unwrap();
    assert_eq!(coordinator.instruction_epoch(), before + 1);
}

#[test]
fn projection_mutation_exclusion() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    coordinator.map(request()).unwrap();
    let mut second = request();
    second.placement = Placement::Fixed(GuestAddress::new(0x3000));
    second.backing = Backing::Anonymous {
        identity: 44,
        shared: false,
    };
    coordinator.map(second).unwrap();
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x3000), 8, Protection::READ)
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let transitioning = Arc::clone(&coordinator);
    let worker = thread::spawn(move || done_tx.send(transitioning.unmap(range())).unwrap());
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(lease);
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    worker.join().unwrap();
}

#[test]
fn projection_shared_coherence() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(71, 4096).unwrap();
    let first = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 1, generation: 1 },
    );
    let second = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        store,
        crate::AddressSpaceId { slot: 2, generation: 1 },
    );
    first.map(request()).unwrap();
    let protection = Protection::READ.union(Protection::WRITE);
    first.map(shared_request(object, 0x3000, protection)).unwrap();
    second.map(shared_request(object, 0x4000, protection)).unwrap();
    let mut lease = first
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x3000), 4, Protection::WRITE)
        .unwrap();
    {
        let mut state = first.host.state.lock().unwrap();
        for (offset, byte) in b"live".iter().enumerate() {
            state.bytes.insert(0x3000 + offset as u64, *byte);
        }
    }
    lease.publish_written().unwrap();
    let mut visible = [0; 4];
    second
        .read(GuestAddress::new(0x4000), &mut visible, Protection::READ)
        .unwrap();
    assert_eq!(&visible, b"live");
}

#[test]
fn full_projection_publication_breaks_shared_exclusive() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(72, 4096).unwrap();
    let writer = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 1, generation: 1 },
    );
    let competitor = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        store,
        crate::AddressSpaceId { slot: 2, generation: 1 },
    );
    writer.map(request()).unwrap();
    let protection = Protection::READ.union(Protection::WRITE);
    writer.map(shared_request(object, 0x3000, protection)).unwrap();
    competitor.map(shared_request(object, 0x4000, protection)).unwrap();
    let (_, exclusive) = competitor
        .load_exclusive(GuestAddress::new(0x4000), 8, false, AtomicOrder::Acquire)
        .unwrap();

    let mut lease = writer
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::READ, 1)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x3008), 8, Protection::WRITE)
        .unwrap();
    {
        let mut state = writer.host.state.lock().unwrap();
        for (offset, byte) in b"changed!".iter().enumerate() {
            state.bytes.insert(0x3008 + offset as u64, *byte);
        }
    }
    lease.publish_written().unwrap();

    assert_eq!(
        competitor.store_exclusive(exclusive, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release),
        Ok(false),
        "Full publication through an additional shared view must break a competing reservation",
    );
}

#[test]
fn projection_dirty_journal_reconciles_only_recorded_ranges() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(72, 4096).unwrap();
    let first = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 3, generation: 1 },
    );
    let second = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        store,
        crate::AddressSpaceId { slot: 4, generation: 1 },
    );
    let protection = Protection::READ.union(Protection::WRITE);
    first.map(shared_request(object, 0x3000, protection)).unwrap();
    second.map(shared_request(object, 0x4000, protection)).unwrap();
    let lease = first
        .project_contiguous(GuestAddress::new(0x3000), 8, Protection::WRITE, 1)
        .unwrap();
    {
        let mut state = first.host.state.lock().unwrap();
        for (offset, byte) in b"skipLIVE".iter().enumerate() {
            state.bytes.insert(0x3000 + offset as u64, *byte);
        }
    }
    let dirty = AddressRange::nonempty(GuestAddress::new(0x3004), 4).unwrap();
    lease.publish_written_ranges(&[dirty]).unwrap();
    let mut visible = [0; 8];
    second
        .read(GuestAddress::new(0x4000), &mut visible, Protection::READ)
        .unwrap();
    assert_eq!(&visible, b"\0\0\0\0LIVE");
}

#[test]
fn coherent_shared_projection_preserves_backing_while_direct_projection_reconciles() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(73, 4096).unwrap();
    let coherent = MappingCoordinator::with_shared_space(
        FakeHost::coherent(),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 5, generation: 1 },
    );
    let direct = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 6, generation: 1 },
    );
    let observer = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        store,
        crate::AddressSpaceId { slot: 7, generation: 1 },
    );
    let protection = Protection::READ.union(Protection::WRITE);
    coherent.map(shared_request(object, 0x3000, protection)).unwrap();
    direct.map(shared_request(object, 0x4000, protection)).unwrap();
    observer.map(shared_request(object, 0x5000, protection)).unwrap();
    let initial = observer.prepare_write(GuestAddress::new(0x5000), 4).unwrap();
    observer.commit_write(initial, b"LIVE").unwrap();

    let lease = coherent
        .project_contiguous(GuestAddress::new(0x3000), 4, Protection::WRITE, 1)
        .unwrap();
    assert_eq!(
        lease.write_publication(GuestAddress::new(0x3000)),
        crate::WritePublication::Exact
    );
    coherent
        .host
        .state
        .lock()
        .unwrap()
        .bytes
        .extend((0..4).map(|offset| (0x3000 + offset, b'X')));
    lease.publish_written().unwrap();
    let mut visible = [0; 4];
    observer
        .read(GuestAddress::new(0x5000), &mut visible, Protection::READ)
        .unwrap();
    assert_eq!(&visible, b"LIVE");

    let lease = direct
        .project_contiguous(GuestAddress::new(0x4000), 4, Protection::WRITE, 1)
        .unwrap();
    direct
        .host
        .state
        .lock()
        .unwrap()
        .bytes
        .extend((0..4).map(|offset| (0x4000 + offset, b'R')));
    lease.publish_written().unwrap();
    observer
        .read(GuestAddress::new(0x5000), &mut visible, Protection::READ)
        .unwrap();
    assert_eq!(&visible, b"RRRR");
}

#[test]
fn projection_dirty_journal_is_bounded_and_rejects_unprojected_ranges() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut mapped = request();
    mapped.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(mapped).unwrap();
    let outside = AddressRange::nonempty(GuestAddress::new(0x3000), 1).unwrap();
    assert_eq!(
        coordinator
            .project_contiguous(GuestAddress::new(0x1000), 8, Protection::WRITE, 1)
            .unwrap()
            .publish_written_ranges(&[outside]),
        Err(MemoryError::NoAddressSpace)
    );
    let dirty = AddressRange::nonempty(GuestAddress::new(0x1000), 1).unwrap();
    assert_eq!(
        coordinator
            .project_contiguous(GuestAddress::new(0x1000), 8, Protection::WRITE, 1)
            .unwrap()
            .publish_written_ranges(&vec![dirty; crate::DIRTY_RANGE_MAXIMUM + 1]),
        Err(MemoryError::ResourceLimit)
    );
    assert!(coordinator.host.state.lock().unwrap().live.is_empty());
}

#[test]
fn projection_dirty_journal_keeps_disjoint_executable_views_exact() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut first = request();
    first.protection = Protection::READ.union(Protection::WRITE).union(Protection::EXECUTE);
    first.backing = Backing::Anonymous {
        identity: 81,
        shared: false,
    };
    coordinator.map(first).unwrap();
    let mut second = first;
    second.placement = Placement::Fixed(GuestAddress::new(0x3000));
    second.backing = Backing::Anonymous {
        identity: 82,
        shared: false,
    };
    coordinator.map(second).unwrap();
    let first_range = AddressRange::nonempty(GuestAddress::new(0x1000), 8).unwrap();
    let second_range = AddressRange::nonempty(GuestAddress::new(0x3000), 8).unwrap();
    let first_token = coordinator.executable_token(first_range, 1);
    let second_token = coordinator.executable_token(second_range, 1);
    let mut lease = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 8, Protection::WRITE, 1)
        .unwrap();
    lease
        .project_additional(GuestAddress::new(0x3000), 8, Protection::WRITE)
        .unwrap();
    let dirty = AddressRange::nonempty(GuestAddress::new(0x1002), 2).unwrap();
    lease.publish_written_ranges(&[dirty]).unwrap();
    assert_ne!(coordinator.executable_token(first_range, 1), first_token);
    assert_eq!(coordinator.executable_token(second_range, 1), second_token);
    let transcript = &coordinator.host.state.lock().unwrap().transcript;
    assert_eq!(
        transcript
            .iter()
            .filter(|entry| entry.starts_with("commit-external:"))
            .count(),
        1
    );
    assert!(transcript.iter().any(|entry| entry.starts_with("rollback:")));
}

/// One guest store is a single admitted interval, so a checkpoint freeze
/// observes it either wholly staged-and-published or not started, never
/// half-applied between staging and publication.
#[test]
fn a_staged_write_span_holds_off_a_checkpoint_freeze_until_it_commits() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    let mut mapping = request();
    mapping.protection = Protection::WRITE;
    coordinator.map(mapping).unwrap();
    let prepared = coordinator.prepare_write_spans(GuestAddress::new(0x1000), 4).unwrap();

    let (finished, completion) = mpsc::channel();
    let freezer = Arc::clone(&coordinator);
    let thread = thread::spawn(move || {
        freezer.freeze_checkpoint();
        finished.send(()).unwrap();
    });

    // The staged span still holds its admission, so the freeze cannot drain.
    assert!(completion.recv_timeout(Duration::from_millis(250)).is_err());

    coordinator.commit_write_spans(prepared, &[1, 2, 3, 4]).unwrap();
    completion.recv_timeout(Duration::from_secs(5)).unwrap();
    thread.join().unwrap();
    coordinator.thaw_checkpoint();

    let state = coordinator.host.state.lock().unwrap();
    assert_eq!(state.bytes.get(&0x1000), Some(&1));
    assert_eq!(state.bytes.get(&0x1003), Some(&4));
}

fn range() -> AddressRange {
    AddressRange::nonempty(GuestAddress::new(0x1000), 4096).unwrap()
}

fn request() -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(0x1000)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ,
        backing: Backing::File {
            identity: FileIdentity { device: 1, object: 2 },
            shared: true,
        },
        backing_offset: 0,
    }
}

#[test]
fn access_fault_prefix() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    let mut denied = request();
    denied.placement = Placement::Fixed(GuestAddress::new(0x2000));
    denied.protection = Protection::NONE;
    coordinator.map(denied).unwrap();

    assert_eq!(
        coordinator.access_prefix(GuestAddress::new(0x1000), 8192, Protection::READ),
        Ok(4096),
    );
    assert_eq!(
        coordinator.access_prefix(GuestAddress::new(0x2000), 4096, Protection::READ),
        Err(MemoryError::NoAddressSpace),
    );
}

#[test]
fn external_prefix_commits() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let state = &coordinator.host.state;

    let result = coordinator
        .external_write(GuestAddress::new(0x1000), 16, || {
            let mut state = state.lock().unwrap();
            state.bytes.insert(0x1000, 1);
            state.bytes.insert(0x1001, 2);
            state.bytes.insert(0x1002, 3);
            Ok::<_, ()>(3)
        })
        .unwrap();

    assert_eq!(result, Ok(3));
    assert!(
        coordinator
            .host
            .state
            .lock()
            .unwrap()
            .transcript
            .iter()
            .any(|entry| { entry.starts_with("commit-external:") && entry.ends_with(":3") })
    );
}

#[test]
fn external_error_rolls() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();

    let result = coordinator.external_write(GuestAddress::new(0x1000), 16, || Err::<usize, _>(7));

    assert_eq!(result, Ok(Err(7)));
    let state = coordinator.host.state.lock().unwrap();
    assert!(state.writes.is_empty());
    assert!(state.transcript.iter().any(|entry| entry.starts_with("rollback:")));
}

#[test]
fn vector_prefix_commits() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    for address in [0x1000, 0x2000] {
        let mut writable = request();
        writable.placement = Placement::Fixed(GuestAddress::new(address));
        writable.protection = Protection::READ.union(Protection::WRITE);
        coordinator.map(writable).unwrap();
    }
    let spans = [
        ExternalSpan {
            address: GuestAddress::new(0x1000),
            length: 4,
        },
        ExternalSpan {
            address: GuestAddress::new(0x2000),
            length: 4,
        },
    ];

    assert_eq!(coordinator.write_vectors(&spans, || Ok::<_, ()>(6)), Ok(Ok(6)));
    let commits: Vec<_> = coordinator
        .host
        .state
        .lock()
        .unwrap()
        .transcript
        .iter()
        .filter(|entry| entry.starts_with("commit-external:"))
        .cloned()
        .collect();
    assert!(commits[0].ends_with(":4"));
    assert!(commits[1].ends_with(":2"));
}

#[test]
fn crosses_mappings() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut first = request();
    first.protection = Protection::EXECUTE;
    coordinator.map(first).unwrap();
    let mut second = request();
    second.placement = Placement::Fixed(GuestAddress::new(0x2000));
    second.backing = Backing::File {
        identity: FileIdentity { device: 3, object: 4 },
        shared: true,
    };
    second.protection = Protection::EXECUTE;
    coordinator.map(second).unwrap();
    {
        let mut state = coordinator.host.state.lock().unwrap();
        state.bytes.insert(0x1ffe, 1);
        state.bytes.insert(0x1fff, 2);
        state.bytes.insert(0x2000, 3);
        state.bytes.insert(0x2001, 4);
    }
    let mut bytes = [0_u8; 4];

    coordinator
        .read_spans(GuestAddress::new(0x1ffe), &mut bytes, Protection::EXECUTE)
        .unwrap();

    assert_eq!(bytes, [1, 2, 3, 4]);
}

#[test]
fn spanning_write_checks_one_mapping_generation() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut first = request();
    first.protection = Protection::WRITE;
    coordinator.map(first).unwrap();
    let mut second = request();
    second.placement = Placement::Fixed(GuestAddress::new(0x2000));
    second.backing = Backing::File {
        identity: FileIdentity { device: 7, object: 8 },
        shared: false,
    };
    second.protection = Protection::WRITE;
    coordinator.map(second).unwrap();
    let prepared = coordinator.prepare_write_spans(GuestAddress::new(0x1ffe), 4).unwrap();

    coordinator.commit_write_spans(prepared, &[1, 2, 3, 4]).unwrap();

    let state = coordinator.host.state.lock().unwrap();
    assert_eq!(state.bytes.get(&0x1ffe), Some(&1));
    assert_eq!(state.bytes.get(&0x1fff), Some(&2));
    assert_eq!(state.bytes.get(&0x2000), Some(&3));
    assert_eq!(state.bytes.get(&0x2001), Some(&4));
    drop(state);
    let stale = coordinator.prepare_write_spans(GuestAddress::new(0x1ffe), 4).unwrap();
    coordinator.protect(range(), Protection::READ).unwrap();
    assert_eq!(
        coordinator.commit_write_spans(stale, &[5, 6, 7, 8]),
        Err(MemoryError::InvariantViolation),
    );
}

#[test]
fn fault_atomic() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut first = request();
    first.protection = Protection::EXECUTE;
    coordinator.map(first).unwrap();
    coordinator.host.state.lock().unwrap().bytes.insert(0x1fff, 7);
    let mut bytes = [0xa5_u8; 2];

    assert_eq!(
        coordinator.read_spans(GuestAddress::new(0x1fff), &mut bytes, Protection::EXECUTE),
        Err(MemoryError::NoAddressSpace),
    );

    assert_eq!(bytes, [0xa5; 2]);
}

#[test]
fn requires_execute() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(request()).unwrap();
    let mut bytes = [0xa5_u8; 1];

    assert_eq!(
        coordinator.read_spans(GuestAddress::new(0x1000), &mut bytes, Protection::EXECUTE),
        Err(MemoryError::NoAddressSpace),
    );
    assert_eq!(bytes, [0xa5]);
}

#[test]
fn short_read_observes_unmap_without_publishing_bytes() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.protection = Protection::EXECUTE;
    coordinator.map(executable).unwrap();
    coordinator.host.state.lock().unwrap().bytes.insert(0x1000, 7);
    let mut bytes = [0_u8; 1];
    coordinator
        .read_spans(GuestAddress::new(0x1000), &mut bytes, Protection::EXECUTE)
        .unwrap();
    assert_eq!(bytes, [7]);

    coordinator.unmap(range()).unwrap();
    bytes = [0xa5];
    assert_eq!(
        coordinator.read_spans(GuestAddress::new(0x1000), &mut bytes, Protection::EXECUTE),
        Err(MemoryError::NoAddressSpace),
    );
    assert_eq!(bytes, [0xa5]);
}

#[test]
fn prepared_write_drop() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let prepared = coordinator.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    drop(prepared);
    assert!(coordinator.host.state.lock().unwrap().live.is_empty());

    let stale = coordinator.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    coordinator.protect(range(), Protection::READ).unwrap();
    assert_eq!(
        coordinator.commit_write(stale, &[1, 2, 3, 4]),
        Err(MemoryError::InvariantViolation),
    );
    assert!(coordinator.host.state.lock().unwrap().live.is_empty());
}

#[test]
fn committed_write_publishes() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let mapped = coordinator.instruction_epoch();
    let prepared = coordinator.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    assert_eq!(coordinator.commit_write(prepared, &[1, 2, 3, 4]).unwrap(), 1);
    assert_eq!(coordinator.observer_epoch(), 1);
    assert_eq!(coordinator.instruction_epoch(), mapped);
    assert!(coordinator.host.state.lock().unwrap().live.is_empty());
}

#[test]
fn executable_write_publishes() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.protection = Protection::WRITE.union(Protection::EXECUTE);
    coordinator.map(executable).unwrap();
    let mapped = coordinator.instruction_epoch();
    let prepared = coordinator.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    coordinator.commit_write(prepared, &[1, 2, 3, 4]).unwrap();
    assert_eq!(coordinator.instruction_epoch(), mapped + 1);
}

#[test]
fn spanning_executable_write_publishes_once() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut first = request();
    first.protection = Protection::WRITE;
    coordinator.map(first).unwrap();
    let mut second = request();
    second.placement = Placement::Fixed(GuestAddress::new(0x2000));
    second.protection = Protection::WRITE.union(Protection::EXECUTE);
    coordinator.map(second).unwrap();
    let mapped = coordinator.instruction_epoch();

    let prepared = coordinator.prepare_write_spans(GuestAddress::new(0x1ffe), 4).unwrap();
    coordinator.commit_write_spans(prepared, &[1, 2, 3, 4]).unwrap();
    assert_eq!(coordinator.instruction_epoch(), mapped + 1);
}

#[test]
fn nonexecutable_write_does_not_publish_instruction() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::WRITE;
    coordinator.map(writable).unwrap();
    let mapped = coordinator.instruction_epoch();
    let prepared = coordinator.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    coordinator.commit_write(prepared, &[1, 2, 3, 4]).unwrap();
    assert_eq!(coordinator.instruction_epoch(), mapped);
}

#[test]
fn atomics_are_transactional() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();

    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &5_u64.to_le_bytes()).unwrap();
    let (loaded, exclusive) = coordinator
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();
    assert_eq!(loaded.low, 5);

    let same_granule = coordinator.prepare_write(GuestAddress::new(0x1010), 1).unwrap();
    coordinator.commit_write(same_granule, &[1]).unwrap();
    assert_eq!(
        coordinator.store_exclusive(exclusive, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release,),
        Ok(false),
    );
    assert_eq!(
        coordinator.fetch_update(
            GuestAddress::new(0x1000),
            8,
            AtomicOperation::Add,
            3,
            AtomicOrder::AcquireRelease,
        ),
        Ok(5),
    );
    assert_eq!(
        coordinator.compare_exchange(
            GuestAddress::new(0x1000),
            8,
            false,
            AtomicValue { low: 8, high: 0 },
            AtomicValue { low: 13, high: 0 },
            AtomicOrder::Relaxed,
        ),
        Ok(AtomicValue { low: 8, high: 0 }),
    );
    let mut bytes = [0_u8; 8];
    coordinator
        .read(GuestAddress::new(0x1000), &mut bytes, Protection::READ)
        .unwrap();
    assert_eq!(u64::from_le_bytes(bytes), 13);
}

/// A write published by another thread must break a monitor this thread armed.
/// Same-granule coverage elsewhere is single-threaded, so it cannot catch an
/// invalidation that is skipped only for writers that did not arm.
#[test]
fn foreign_thread_write_breaks_exclusive() {
    let coordinator = std::sync::Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &5_u64.to_le_bytes()).unwrap();

    let (loaded, exclusive) = coordinator
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();
    assert_eq!(loaded.low, 5);

    // The interfering write lands on a different address inside the same
    // 64-byte reservation granule, on a thread that armed nothing.
    let writer = std::sync::Arc::clone(&coordinator);
    std::thread::spawn(move || {
        let prepared = writer.prepare_write(GuestAddress::new(0x1008), 8).unwrap();
        writer.commit_write(prepared, &7_u64.to_le_bytes()).unwrap();
    })
    .join()
    .unwrap();

    assert_eq!(
        coordinator.store_exclusive(exclusive, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release),
        Ok(false),
        "a foreign thread's same-granule write must break the reservation",
    );
    let mut bytes = [0_u8; 8];
    coordinator
        .read(GuestAddress::new(0x1000), &mut bytes, Protection::READ)
        .unwrap();
    assert_eq!(u64::from_le_bytes(bytes), 5, "the failed store must not have published");
}

#[test]
fn disjoint_write_preserves() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &5_u64.to_le_bytes()).unwrap();

    let (_, exclusive) = coordinator
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();
    let disjoint = coordinator.prepare_write(GuestAddress::new(0x1080), 8).unwrap();
    coordinator.commit_write(disjoint, &7_u64.to_le_bytes()).unwrap();
    assert_eq!(
        coordinator.store_exclusive(exclusive, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release),
        Ok(true),
    );
}

#[test]
fn restore_clears_exclusive() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &5_u64.to_le_bytes()).unwrap();
    let (_, exclusive) = coordinator
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();

    for value in [7_u64, 5] {
        let write = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
        coordinator.commit_write(write, &value.to_le_bytes()).unwrap();
    }
    assert_eq!(
        coordinator.store_exclusive(exclusive, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release),
        Ok(false),
    );
}

#[test]
fn exclusive_contention_progresses() {
    const THREADS: usize = 4;
    const UPDATES: usize = 2_000;
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(writable).unwrap();
    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &0_u64.to_le_bytes()).unwrap();

    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let coordinator = Arc::clone(&coordinator);
            thread::spawn(move || {
                for _ in 0..UPDATES {
                    loop {
                        let (loaded, reservation) = coordinator
                            .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
                            .unwrap();
                        if coordinator
                            .store_exclusive(
                                reservation,
                                AtomicValue {
                                    low: loaded.low + 1,
                                    high: 0,
                                },
                                AtomicOrder::Release,
                            )
                            .unwrap()
                        {
                            break;
                        }
                        thread::yield_now();
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let mut bytes = [0_u8; 8];
    coordinator
        .read(GuestAddress::new(0x1000), &mut bytes, Protection::READ)
        .unwrap();
    assert_eq!(u64::from_le_bytes(bytes), (THREADS * UPDATES) as u64);
}

#[test]
fn shared_alias_invalidates() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(7, 4096).unwrap();
    let request_at = |address| MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Shared(SharedBackingRef {
            object,
            offset: 0,
            length: 4096,
            write_shared: true,
        }),
        backing_offset: 0,
    };
    let first = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), Arc::clone(&store));
    let second = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store);
    first.map(request_at(0x1000)).unwrap();
    second.map(request_at(0x3000)).unwrap();
    let (_, reservation) = first
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();
    second
        .store_ordered(GuestAddress::new(0x3000), 8, 5, AtomicOrder::Release)
        .unwrap();
    assert!(
        !first
            .store_exclusive(reservation, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release)
            .unwrap()
    );
}

#[test]
fn shared_executable_alias_invalidates_instructions() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(7, 4096).unwrap();
    let coordinator = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store);
    coordinator
        .map(shared_request(
            object,
            0x1000,
            Protection::READ.union(Protection::WRITE),
        ))
        .unwrap();
    coordinator
        .map(shared_request(
            object,
            0x3000,
            Protection::READ.union(Protection::EXECUTE),
        ))
        .unwrap();
    let mapped = coordinator.instruction_epoch();

    let prepared = coordinator.prepare_write_spans(GuestAddress::new(0x1000), 4).unwrap();
    coordinator.commit_write_spans(prepared, &[1, 2, 3, 4]).unwrap();

    assert_eq!(coordinator.instruction_epoch(), mapped + 1);
    let alias = AddressRange::nonempty(GuestAddress::new(0x3000), 4).unwrap();
    assert_eq!(coordinator.executable_token(alias, 1).version, mapped + 1);
}

#[test]
fn private_copies_isolated() {
    let request = |address| MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Anonymous {
            identity: 77,
            shared: false,
        },
        backing_offset: 0,
    };
    let first = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let second = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    first.map(request(0x1000)).unwrap();
    second.map(request(0x1000)).unwrap();
    let (_, reservation) = first
        .load_exclusive(GuestAddress::new(0x1000), 8, false, AtomicOrder::Acquire)
        .unwrap();
    second
        .store_ordered(GuestAddress::new(0x1000), 8, 5, AtomicOrder::Release)
        .unwrap();
    assert!(
        first
            .store_exclusive(reservation, AtomicValue { low: 9, high: 0 }, AtomicOrder::Release)
            .unwrap()
    );
}

#[test]
fn reused_address_space() {
    let first = MappingCoordinator::with_address_space(
        FakeHost::failing(usize::MAX),
        crate::AddressSpaceId { slot: 3, generation: 7 },
    );
    let second = MappingCoordinator::with_address_space(
        FakeHost::failing(usize::MAX),
        crate::AddressSpaceId { slot: 3, generation: 8 },
    );
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    first.map(writable).unwrap();
    second.map(writable).unwrap();
    let address = GuestAddress::new(0x1000);
    let left = first
        .ledger()
        .futex_identity(first.address_space().unwrap(), address, true, crate::FutexAccess::Read)
        .unwrap();
    let right = second
        .ledger()
        .futex_identity(second.address_space().unwrap(), address, true, crate::FutexAccess::Read)
        .unwrap();
    assert_ne!(left, right);
}

#[test]
fn private_shared_operation() {
    let coordinator = MappingCoordinator::with_address_space(
        FakeHost::failing(usize::MAX),
        crate::AddressSpaceId { slot: 4, generation: 1 },
    );
    let mut writable = request();
    writable.protection = Protection::READ.union(Protection::WRITE);
    writable.backing = Backing::Anonymous {
        identity: 9,
        shared: false,
    };
    coordinator.map(writable).unwrap();
    assert_eq!(
        coordinator.ledger().futex_identity(
            coordinator.address_space().unwrap(),
            GuestAddress::new(0x1000),
            false,
            crate::FutexAccess::Read,
        ),
        Some(crate::FutexIdentity::Private {
            address_space: crate::AddressSpaceId { slot: 4, generation: 1 },
            address: 0x1000,
        }),
    );
}

fn shared_request(id: crate::SharedObjectId, address: u64, protection: Protection) -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length: 4096,
        alignment: 4096,
        protection,
        backing: Backing::Shared(SharedBackingRef {
            object: id,
            offset: 0,
            length: 4096,
            write_shared: true,
        }),
        backing_offset: 0,
    }
}

#[test]
fn shared_write_visible() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(7, 4096).unwrap();
    let first = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        Arc::clone(&store),
        crate::AddressSpaceId { slot: 1, generation: 1 },
    );
    let second = MappingCoordinator::with_shared_space(
        FakeHost::failing(usize::MAX),
        store,
        crate::AddressSpaceId { slot: 2, generation: 1 },
    );
    let protection = Protection::READ.union(Protection::WRITE);
    first.map(shared_request(object, 0x1000, protection)).unwrap();
    second.map(shared_request(object, 0x2000, protection)).unwrap();

    let write = first.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    first.commit_write(write, b"fork").unwrap();
    let mut visible = [0; 4];
    second
        .read(GuestAddress::new(0x2000), &mut visible, Protection::READ)
        .unwrap();
    assert_eq!(&visible, b"fork");

    let mut private = request();
    private.protection = protection;
    second.map(private).unwrap();
    let mut isolated = [0; 4];
    second
        .read(GuestAddress::new(0x1000), &mut isolated, Protection::READ)
        .unwrap();
    assert_eq!(isolated, [0; 4]);
}

// hl-lint: visual-section
fn batch() -> MappingBatch {
    let mut batch = MappingBatch::new();
    batch.push(MappingOperation::Map(request()));
    batch.push(MappingOperation::Protect(
        range(),
        Protection::READ.union(Protection::WRITE),
    ));
    batch.push(MappingOperation::Unmap(range()));
    batch
}

#[test]
fn every_host_failure() {
    for fail_at in 0..=3 {
        let coordinator = MappingCoordinator::new(FakeHost {
            state: Mutex::new(FakeState::default()),
            fail_at,
            coherent_projection: false,
        });
        assert_eq!(coordinator.apply(&batch()), Err(MemoryError::InvariantViolation));
        assert_eq!(coordinator.ledger().generation(), 0);
        assert!(coordinator.ledger().regions().is_empty());
        let state = coordinator
            .host
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.live.is_empty());
        let rollbacks: Vec<_> = state
            .transcript
            .iter()
            .filter(|entry| entry.starts_with("rollback"))
            .collect();
        assert!(rollbacks.windows(2).all(|pair| pair[0] > pair[1]));
    }
}

#[test]
fn successful_batch_has() {
    let coordinator = MappingCoordinator::new(FakeHost {
        state: Mutex::new(FakeState::default()),
        fail_at: usize::MAX,
        coherent_projection: false,
    });
    assert_eq!(coordinator.apply(&batch()), Ok(vec![GuestAddress::new(0x1000)]));
    assert_eq!(coordinator.ledger().generation(), 1);
    assert!(coordinator.ledger().regions().is_empty());
    let state = coordinator
        .host
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(state.transcript, ["map", "protect", "unmap", "commit"]);
    assert!(state.live.is_empty());
}

#[test]
fn concurrent_readers_observe() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost {
        state: Mutex::new(FakeState::default()),
        fail_at: usize::MAX,
        coherent_projection: false,
    }));
    coordinator.map(request()).unwrap();
    let reader = coordinator.clone();
    let worker = thread::spawn(move || {
        for _ in 0..2_000 {
            let protection = reader.ledger().regions()[0].protection();
            assert!(protection == Protection::READ || protection == Protection::WRITE);
        }
    });
    let mut change = MappingBatch::new();
    change.push(MappingOperation::Protect(range(), Protection::WRITE));
    coordinator.apply(&change).unwrap();
    worker.join().unwrap();
}

#[test]
fn shared_backing_is() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let stale = store.create(1, 4096).unwrap();
    store.remove(stale).unwrap();
    let coordinator = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store.clone());
    assert_eq!(
        coordinator.map(shared_request(stale, 0x1000, Protection::READ)),
        Err(MemoryError::Shared(SharedError::NotFound))
    );
    assert!(coordinator.host.state.lock().unwrap().transcript.is_empty());

    let truncated = store.create(1, 8192).unwrap();
    let request = MapRequest {
        backing: Backing::Shared(SharedBackingRef {
            object: truncated,
            offset: 4096,
            length: 4096,
            write_shared: true,
        }),
        ..shared_request(truncated, 0x1000, Protection::READ)
    };
    store.resize(truncated, 4096).unwrap();
    assert_eq!(coordinator.map(request), Err(MemoryError::Shared(SharedError::Range)));
    assert!(coordinator.host.state.lock().unwrap().transcript.is_empty());
}

#[test]
fn write_seals_are() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let id = store.create(1, 4096).unwrap();
    store
        .add_seals(id, SharedSeal::from_bits(SharedSeal::FUTURE_WRITE))
        .unwrap();
    let coordinator = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store);
    assert_eq!(
        coordinator.map(shared_request(id, 0x1000, Protection::WRITE)),
        Err(MemoryError::Shared(SharedError::Sealed))
    );
    assert!(coordinator.host.state.lock().unwrap().transcript.is_empty());
}

#[test]
fn aliases_hold_independent() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let id = store.create(1, 4096).unwrap();
    let coordinator = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store.clone());
    coordinator.map(shared_request(id, 0x1000, Protection::READ)).unwrap();
    coordinator.map(shared_request(id, 0x3000, Protection::READ)).unwrap();
    assert_eq!(store.pin_count(id), Ok(2));

    coordinator.unmap(range()).unwrap();
    assert_eq!(store.pin_count(id), Ok(1));
    coordinator
        .unmap(AddressRange::nonempty(GuestAddress::new(0x3000), 4096).unwrap())
        .unwrap();
    assert_eq!(store.pin_count(id), Ok(0));
}

#[test]
fn every_stage_and() {
    for fail_at in 0..=1 {
        let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let id = store.create(1, 4096).unwrap();
        let coordinator = MappingCoordinator::with_shared(FakeHost::failing(fail_at), store.clone());
        assert_eq!(
            coordinator.map(shared_request(id, 0x1000, Protection::READ)),
            Err(MemoryError::InvariantViolation)
        );
        assert_eq!(store.pin_count(id), Ok(0));
        assert!(coordinator.ledger().regions().is_empty());
    }

    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let id = store.create(1, 4096).unwrap();
    let coordinator = MappingCoordinator::with_shared(FakeHost::failing(2), store.clone());
    coordinator.map(shared_request(id, 0x1000, Protection::READ)).unwrap();
    assert_eq!(coordinator.unmap(range()), Err(MemoryError::InvariantViolation));
    assert_eq!(store.pin_count(id), Ok(1));
    assert_eq!(coordinator.ledger().regions().len(), 1);
}

#[test]
fn snapshot_restore_rebinds() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let id = store.create(1, 4096).unwrap();
    let parent = MappingCoordinator::with_shared(FakeHost::failing(usize::MAX), store.clone());
    parent.map(shared_request(id, 0x1000, Protection::READ)).unwrap();
    let child = MappingCoordinator::restore(FakeHost::failing(usize::MAX), store.clone(), parent.snapshot()).unwrap();
    assert_eq!(store.pin_count(id), Ok(2));
    assert_eq!(child.ledger().regions(), parent.ledger().regions());
    drop(child);
    assert_eq!(store.pin_count(id), Ok(1));

    let snapshot = parent.snapshot();
    store.remove(id).unwrap();
    assert_eq!(
        MappingCoordinator::restore(FakeHost::failing(usize::MAX), store, snapshot).unwrap_err(),
        MemoryError::Shared(SharedError::NotFound)
    );
}

#[test]
fn frozen_coordinator_blocks() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    coordinator.freeze_checkpoint();
    let worker_coordinator = coordinator.clone();
    let (sent, received) = mpsc::channel();
    let worker = thread::spawn(move || {
        worker_coordinator.map(request()).unwrap();
        sent.send(()).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    assert!(coordinator.checkpoint_snapshot().unwrap().regions.is_empty());
    coordinator.thaw_checkpoint();
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
    assert_eq!(coordinator.ledger().regions().len(), 1);
}

#[test]
fn checkpoint_request_invalidates_live_projection_before_freeze_completes() {
    let coordinator = Arc::new(MappingCoordinator::new(FakeHost::failing(usize::MAX)));
    coordinator.map(request()).unwrap();
    let projection = coordinator
        .project_contiguous(GuestAddress::new(0x1000), 1, Protection::READ, 1)
        .unwrap();
    let continuation = projection.checkpoint_continuation();
    let freezer = Arc::clone(&coordinator);
    let (sent, received) = mpsc::channel();
    let worker = thread::spawn(move || {
        freezer.freeze_checkpoint();
        sent.send(()).unwrap();
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while continuation.is_current() && std::time::Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(!continuation.is_current());
    assert!(received.try_recv().is_err());

    drop(projection);
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    coordinator.thaw_checkpoint();
    assert!(!continuation.is_current());
    worker.join().unwrap();
}

#[test]
fn unmapping_and_reusing_a_range_supersedes_its_executable_token() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    coordinator.map(executable).unwrap();
    let range = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let before = coordinator.executable_token(range, 1);

    coordinator
        .unmap(AddressRange::nonempty(GuestAddress::new(0x3000), 4096).unwrap())
        .unwrap();
    coordinator.map(executable).unwrap();

    assert_ne!(coordinator.executable_token(range, 1), before);
}

#[test]
fn reprotecting_written_data_as_executable_supersedes_its_token() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut data = request();
    data.placement = Placement::Fixed(GuestAddress::new(0x3000));
    data.protection = Protection::READ.union(Protection::WRITE);
    coordinator.map(data).unwrap();
    let range = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let before = coordinator.executable_token(range, 1);
    let prepared = coordinator.prepare_write(GuestAddress::new(0x3000), 4).unwrap();
    coordinator.commit_write(prepared, &[1, 2, 3, 4]).unwrap();
    assert_eq!(coordinator.executable_token(range, 1), before);

    coordinator
        .protect(
            AddressRange::nonempty(GuestAddress::new(0x3000), 4096).unwrap(),
            Protection::READ.union(Protection::EXECUTE),
        )
        .unwrap();

    assert_ne!(coordinator.executable_token(range, 1), before);
}

#[test]
fn a_mapping_transition_supersedes_its_own_range_and_leaves_the_rest_alone() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    for base in [0x3000, 0x5000] {
        executable.placement = Placement::Fixed(GuestAddress::new(base));
        coordinator.map(executable).unwrap();
    }
    let transitioned = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let untouched = AddressRange::nonempty(GuestAddress::new(0x5000), 16).unwrap();
    let transitioned_before = coordinator.executable_token(transitioned, 1);
    let untouched_before = coordinator.executable_token(untouched, 1);

    coordinator
        .unmap(AddressRange::nonempty(GuestAddress::new(0x3000), 4096).unwrap())
        .unwrap();

    assert_ne!(coordinator.executable_token(transitioned, 1), transitioned_before);
    assert_eq!(coordinator.executable_token(untouched, 1), untouched_before);
}

#[test]
fn remapping_the_same_address_supersedes_it_for_every_transition_kind() {
    let range = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let page = AddressRange::nonempty(GuestAddress::new(0x3000), 4096).unwrap();
    let mut executable = request();
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));

    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(executable).unwrap();
    let before = coordinator.executable_token(range, 1);
    coordinator.unmap(page).unwrap();
    coordinator.map(executable).unwrap();
    assert_ne!(coordinator.executable_token(range, 1), before);

    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(executable).unwrap();
    let before = coordinator.executable_token(range, 1);
    coordinator.map(executable).unwrap();
    assert_ne!(coordinator.executable_token(range, 1), before);

    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(executable).unwrap();
    let before = coordinator.executable_token(range, 1);
    coordinator
        .protect(page, Protection::READ.union(Protection::WRITE))
        .unwrap();
    assert_ne!(coordinator.executable_token(range, 1), before);

    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    coordinator.map(executable).unwrap();
    let before = coordinator.executable_token(range, 1);
    let mut batch = MappingBatch::default();
    batch.push(MappingOperation::Unmap(page));
    coordinator.apply(&batch).unwrap();
    assert_ne!(coordinator.executable_token(range, 1), before);
}

/// A guest `ic ivau` names an address whose bytes this address space may never have written:
/// a peer address space can rewrite a shared executable object, and only the executing space's
/// own maintenance supersedes its translations. It must supersede that page and no other.
#[test]
fn addressed_instruction_publication_supersedes_only_the_named_page() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.length = 0x2000;
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    coordinator.map(executable).unwrap();
    let named = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let other = AddressRange::nonempty(GuestAddress::new(0x4000), 16).unwrap();
    let named_before = coordinator.executable_token(named, 1);
    let other_before = coordinator.executable_token(other, 1);

    coordinator.publish_instruction_at(0x3040);

    assert_ne!(coordinator.executable_token(named, 1), named_before);
    assert_eq!(coordinator.executable_token(other, 1), other_before);
}

#[test]
fn instruction_publication_supersedes_pages_which_never_took_a_write() {
    let coordinator = MappingCoordinator::new(FakeHost::failing(usize::MAX));
    let mut executable = request();
    executable.placement = Placement::Fixed(GuestAddress::new(0x3000));
    executable.protection = Protection::READ.union(Protection::EXECUTE);
    coordinator.map(executable).unwrap();
    let range = AddressRange::nonempty(GuestAddress::new(0x3000), 16).unwrap();
    let before = coordinator.executable_token(range, 1);

    coordinator.publish_instruction();

    assert_ne!(coordinator.executable_token(range, 1), before);
}
