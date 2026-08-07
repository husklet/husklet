//! Proves the coordinator's guest read-modify-write cannot lose an update a
//! peer host thread makes with a host atomic on the same storage, which is what
//! native execution does for every guest atomic it admits.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use hl_isa::{AddressRange, GuestAddress};

use crate::{
    AtomicOperation, AtomicOrder, AtomicValue, Backing, FileIdentity, MapRequest, MappingCoordinator, MappingHost,
    MemoryAccessHost, MemoryError, Placement, Protection,
};

const BASE: u64 = 0x1000;
const LENGTH: u64 = 4096;
const CELLS: usize = (LENGTH / 8) as usize;

/// Storage that is one addressable array of host atomic cells, standing in for
/// the arena. Plain writes go through it byte at a time, so a coordinator
/// read-modify-write is only indivisible if it reaches `compare_exchange_atomic`.
#[derive(Debug)]
struct ArenaHost {
    cells: Vec<AtomicU64>,
    writes: std::sync::Mutex<std::collections::BTreeMap<u64, AddressRange>>,
    next: std::sync::atomic::AtomicU64,
}

impl ArenaHost {
    fn new() -> Self {
        Self {
            cells: (0..CELLS).map(|_| AtomicU64::new(0)).collect(),
            writes: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn site(offset: u64, width: u64) -> (usize, u32, u64) {
        let relative = offset - BASE;
        let shift = ((relative % 8) * 8) as u32;
        let mask = if width == 8 { u64::MAX } else { ((1 << (width * 8)) - 1) << shift };
        ((relative / 8) as usize, shift, mask)
    }

    /// One genuinely indivisible compare-exchange of a subword, performed by a
    /// host atomic on the enclosing cell.
    fn cas(&self, offset: u64, width: u64, expected: u64, replacement: u64) -> u64 {
        let (index, shift, mask) = Self::site(offset, width);
        let cell = &self.cells[index];
        let mut current = cell.load(Ordering::Acquire);
        loop {
            let observed = (current & mask) >> shift;
            if observed != expected {
                return observed;
            }
            let next = (current & !mask) | ((replacement << shift) & mask);
            match cell.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return observed,
                Err(actual) => current = actual,
            }
        }
    }

    fn load(&self, offset: u64, width: u64) -> u64 {
        let (index, shift, mask) = Self::site(offset, width);
        (self.cells[index].load(Ordering::Acquire) & mask) >> shift
    }

    /// The peer that stands in for native execution: a host atomic increment.
    fn peer_increment(&self, offset: u64, width: u64) {
        let limit = if width == 8 { u64::MAX } else { (1 << (width * 8)) - 1 };
        loop {
            let observed = self.load(offset, width);
            if self.cas(offset, width, observed, observed.wrapping_add(1) & limit) == observed {
                return;
            }
        }
    }

    fn reservation(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl MappingHost for ArenaHost {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        Ok(self.reservation())
    }
    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        Ok(self.reservation())
    }
    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        Ok(self.reservation())
    }
    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
}

impl MemoryAccessHost for ArenaHost {
    type Projection = u64;

    fn read(&self, range: AddressRange, output: &mut [u8], _: Protection) -> Result<(), MemoryError> {
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = self.load(range.start().get() + index as u64, 1) as u8;
        }
        Ok(())
    }

    fn prepare_write(&self, range: AddressRange) -> Result<u64, MemoryError> {
        let token = self.reservation();
        self.writes.lock().unwrap().insert(token, range);
        Ok(token)
    }

    fn commit_write(&self, reservation: u64, input: &[u8]) -> Result<(), MemoryError> {
        let range = self
            .writes
            .lock()
            .unwrap()
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        for (index, byte) in input.iter().enumerate() {
            let offset = range.start().get() + index as u64;
            loop {
                let observed = self.load(offset, 1);
                if self.cas(offset, 1, observed, u64::from(*byte)) == observed {
                    break;
                }
            }
        }
        Ok(())
    }

    fn rollback_write(&self, reservation: u64) {
        self.writes.lock().unwrap().remove(&reservation);
    }

    fn compare_exchange_atomic(
        &self,
        range: AddressRange,
        expected: u64,
        replacement: u64,
    ) -> Result<Option<u64>, MemoryError> {
        Ok(Some(self.cas(range.start().get(), range.length(), expected, replacement)))
    }
}

fn writable() -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(GuestAddress::new(BASE)),
        length: LENGTH,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::File {
            identity: FileIdentity { device: 1, object: 2 },
            shared: false,
        },
        backing_offset: 0,
    }
}

/// A byte counter wraps at 256, so it runs fewer but repeated rounds; every
/// other width totals well below its wrap point.
fn plan(width: u8) -> (u64, u32) {
    if width == 1 { (120, 25) } else { (20_000, 1) }
}

/// Runs `rounds` coordinator updates against the same number of peer host
/// atomics on one word, and returns the word.
fn race<F>(width: u8, rounds: u64, mut step: F) -> u64
where
    F: FnMut(&MappingCoordinator<ArenaHost>, GuestAddress),
{
    let coordinator = Arc::new(MappingCoordinator::new(ArenaHost::new()));
    coordinator.map(writable()).unwrap();
    let address = GuestAddress::new(BASE + 64);
    let started = Arc::new(AtomicBool::new(false));

    let peer = Arc::clone(&coordinator);
    let flag = Arc::clone(&started);
    let width_bytes = u64::from(width);
    let worker = std::thread::spawn(move || {
        flag.store(true, Ordering::Release);
        for _ in 0..rounds {
            peer.host.host.peer_increment(address.get(), width_bytes);
        }
    });

    while !started.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }
    for _ in 0..rounds {
        step(&coordinator, address);
    }
    worker.join().unwrap();
    coordinator.host.host.load(address.get(), width_bytes)
}

#[test]
fn fetch_update_does_not_lose_a_peer_host_atomic() {
    let mut lost = Vec::new();
    for width in [1_u8, 2, 4, 8] {
        let (rounds, repeats) = plan(width);
        for _ in 0..repeats {
            let observed = race(width, rounds, |coordinator, address| {
                coordinator
                    .fetch_update(address, width, AtomicOperation::Add, 1, AtomicOrder::AcquireRelease)
                    .unwrap();
            });
            if observed != 2 * rounds {
                lost.push(format!("width {width}: {observed} != {}", 2 * rounds));
            }
        }
    }
    assert!(lost.is_empty(), "{lost:?}");
}

#[test]
fn compare_exchange_does_not_lose_a_peer_host_atomic() {
    let mut lost = Vec::new();
    for width in [1_u8, 2, 4, 8] {
        let limit = if width == 8 { u64::MAX } else { (1 << (u64::from(width) * 8)) - 1 };
        let (rounds, repeats) = plan(width);
        for _ in 0..repeats {
        let observed = race(width, rounds, |coordinator, address| {
            let mut current = AtomicValue { low: 0, high: 0 };
            loop {
                let seen = coordinator
                    .compare_exchange(
                        address,
                        width,
                        false,
                        current,
                        AtomicValue {
                            low: current.low.wrapping_add(1) & limit,
                            high: 0,
                        },
                        AtomicOrder::AcquireRelease,
                    )
                    .unwrap();
                if seen == current {
                    return;
                }
                current = seen;
            }
        });
        if observed != 2 * rounds {
            lost.push(format!("width {width}: {observed} != {}", 2 * rounds));
        }
        }
    }
    assert!(lost.is_empty(), "{lost:?}");
}
