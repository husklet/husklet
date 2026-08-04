use crate::{Backing, FileIdentity, MapRequest, MemoryError, MemoryLedger, Placement, Protection};
use hl_isa::{AddressRange, GuestAddress};
use std::collections::BTreeMap;

const PAGE: u64 = 4096;
const FILE: Backing = Backing::File {
    identity: FileIdentity { device: 7, object: 11 },
    shared: true,
};

fn address(value: u64) -> GuestAddress {
    GuestAddress::new(value)
}

fn range(start: u64, length: u64) -> AddressRange {
    AddressRange::nonempty(address(start), length).unwrap()
}

fn request(start: u64, length: u64, offset: u64, protection: Protection) -> MapRequest {
    MapRequest {
        placement: Placement::Fixed(address(start)),
        length,
        alignment: PAGE,
        protection,
        backing: FILE,
        backing_offset: offset,
    }
}

#[test]
fn map_orders_and() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x3000, PAGE, PAGE * 2, Protection::READ)).unwrap();
    ledger.map(request(0x1000, PAGE * 2, 0, Protection::READ)).unwrap();
    let regions = ledger.regions();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].range(), range(0x1000, PAGE * 3));
    assert_eq!(regions[0].backing_offset(), 0);
    assert_eq!(ledger.validate(), Ok(()));
}

#[test]
fn unmap_middle_splits() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x1000, PAGE * 3, PAGE, Protection::READ)).unwrap();
    ledger.unmap(range(0x2000, PAGE)).unwrap();
    let regions = ledger.regions();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].backing_offset(), PAGE);
    assert_eq!(regions[1].backing_offset(), PAGE * 3);
    assert_eq!(ledger.resolve(address(0x2000), Protection::READ), None);
}

#[test]
fn fixed_map_replaces() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x1000, PAGE * 3, 0, Protection::READ)).unwrap();
    ledger.map(request(0x2000, PAGE, PAGE, Protection::READ)).unwrap();
    assert_eq!(ledger.regions().len(), 1);
    assert_eq!(ledger.regions()[0].range(), range(0x1000, PAGE * 3));
}

#[test]
fn anonymous_charge_provenance_tracks_fixed_replacement_and_unmap() {
    let ledger = MemoryLedger::new();
    let anonymous = MapRequest {
        placement: Placement::Fixed(address(0x4000)),
        length: PAGE * 2,
        alignment: PAGE,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Anonymous {
            identity: 31,
            shared: false,
        },
        backing_offset: 0,
    };
    ledger.map_charged(anonymous, 5000).unwrap();
    assert!(ledger.regions()[0].reserved());
    assert_eq!(ledger.regions()[0].charge().unwrap().length(), 5000);

    ledger.map(request(0x5000, PAGE, 0, Protection::READ)).unwrap();
    let regions = ledger.regions();
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].charge().unwrap().length(), PAGE);
    assert!(!regions[1].reserved());
    assert_eq!(regions[1].charge(), None);

    ledger.unmap(range(0x4000, PAGE)).unwrap();
    assert!(ledger.regions().iter().all(|region| region.charge().is_none()));
}

#[test]
fn charge_provenance_is_anonymous_only() {
    let ledger = MemoryLedger::new();
    assert_eq!(
        ledger.map_charged(request(0x4000, PAGE, 0, Protection::READ), 1),
        Err(MemoryError::InvariantViolation),
    );
    assert!(ledger.regions().is_empty());
}

#[test]
fn protect_splits_exactly() {
    let ledger = MemoryLedger::new();
    let read_write = Protection::READ.union(Protection::WRITE);
    ledger.map(request(0x1000, PAGE * 3, 0, Protection::READ)).unwrap();
    ledger.protect(range(0x2000, PAGE), read_write).unwrap();
    let regions = ledger.regions();
    assert_eq!(regions.len(), 3);
    assert_eq!(regions[1].protection(), read_write);
    assert_eq!(regions[1].backing_offset(), PAGE);
    ledger.protect(range(0x2000, PAGE), Protection::READ).unwrap();
    assert_eq!(ledger.regions().len(), 1);
}

#[test]
fn protect_rejects_unmapped_holes_without_mutation() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x1000, PAGE, 0, Protection::READ)).unwrap();
    ledger.map(request(0x3000, PAGE, PAGE, Protection::READ)).unwrap();
    let before = ledger.regions();
    let generation = ledger.generation();
    assert_eq!(
        ledger.protect(range(0x1000, PAGE * 3), Protection::WRITE),
        Err(MemoryError::Unmapped)
    );
    assert_eq!(ledger.regions(), before);
    assert_eq!(ledger.generation(), generation);
}

#[test]
fn failed_noreplace_is() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x4000, PAGE * 2, 0, Protection::READ)).unwrap();
    let before = ledger.regions();
    let generation = ledger.generation();
    let mut replacement = request(0x5000, PAGE, PAGE * 10, Protection::WRITE);
    replacement.placement = Placement::FixedNoReplace(address(0x5000));
    assert_eq!(ledger.map(replacement), Err(MemoryError::AlreadyMapped));
    assert_eq!(ledger.regions(), before);
    assert_eq!(ledger.generation(), generation);
}

fn anonymous_request(hint: Option<GuestAddress>, identity: u64) -> MapRequest {
    MapRequest {
        placement: Placement::Anywhere {
            minimum: address(0x1000),
            maximum: address(0x9000),
            hint,
        },
        length: PAGE,
        alignment: PAGE,
        protection: Protection::READ,
        backing: Backing::Anonymous {
            identity,
            shared: false,
        },
        backing_offset: 0,
    }
}

#[test]
fn anywhere_prefers_valid() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x3000, PAGE * 2, 0, Protection::READ)).unwrap();
    assert_eq!(
        ledger.map(anonymous_request(Some(address(0x7000)), 1)).unwrap(),
        address(0x7000)
    );
    assert_eq!(
        ledger.map(anonymous_request(Some(address(0x3000)), 2)).unwrap(),
        address(0x1000)
    );
}

#[test]
fn anywhere_honors_large() {
    let ledger = MemoryLedger::new();
    let mut mapping = anonymous_request(None, 3);
    mapping.placement = Placement::Anywhere {
        minimum: address(0x1001),
        maximum: address(0x10_000),
        hint: None,
    };
    mapping.alignment = 0x4000;
    assert_eq!(ledger.map(mapping).unwrap(), address(0x4000));
}

#[test]
fn rejects_empty_unaligned() {
    let ledger = MemoryLedger::new();
    assert_eq!(
        ledger.map(request(0x1000, 0, 0, Protection::READ)),
        Err(MemoryError::EmptyRange)
    );
    assert_eq!(
        ledger.map(request(0x1001, PAGE, 0, Protection::READ)),
        Err(MemoryError::Unaligned)
    );
    assert_eq!(
        ledger.map(request(0x1000, PAGE - 1, 0, Protection::READ)),
        Err(MemoryError::Unaligned)
    );
    assert!(
        ledger
            .map(request(u64::MAX - PAGE + 1, PAGE, 0, Protection::READ))
            .is_err()
    );
    assert!(ledger.regions().is_empty());
}

#[test]
fn resolve_checks_permissions() {
    let ledger = MemoryLedger::new();
    let rx = Protection::READ.union(Protection::EXECUTE);
    ledger.map(request(0x8000, PAGE * 2, PAGE, rx)).unwrap();
    let resolution = ledger.resolve(address(0x8ffc), Protection::EXECUTE).unwrap();
    assert_eq!(resolution.backing_offset, PAGE + 0xffc);
    assert_eq!(resolution.contiguous, PAGE + 4);
    assert_eq!(ledger.resolve(address(0x8000), Protection::WRITE), None);
}

#[test]
fn different_file_identity() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x1000, PAGE, 0, Protection::READ)).unwrap();
    let mut other = request(0x2000, PAGE, PAGE, Protection::READ);
    other.backing = Backing::File {
        identity: FileIdentity { device: 7, object: 12 },
        shared: true,
    };
    ledger.map(other).unwrap();
    let mut private = request(0x3000, PAGE, PAGE * 2, Protection::READ);
    private.backing = Backing::File {
        identity: FileIdentity { device: 7, object: 12 },
        shared: false,
    };
    ledger.map(private).unwrap();
    assert_eq!(ledger.regions().len(), 3);
    assert_eq!(ledger.validate(), Ok(()));
}

#[test]
fn model_sequence_preserves() {
    let ledger = MemoryLedger::new();
    for page in (0..64).step_by(2) {
        ledger
            .map(request(0x10_000 + page * PAGE, PAGE, page * PAGE, Protection::READ))
            .unwrap();
        ledger.validate().unwrap();
    }
    for page in 0..64 {
        let operation = range(0x10_000 + page * PAGE, PAGE);
        if page % 3 == 0 {
            ledger.unmap(operation).unwrap();
        } else if page % 2 == 0 {
            ledger
                .protect(operation, Protection::READ.union(Protection::WRITE))
                .unwrap();
        } else {
            assert_eq!(
                ledger.protect(operation, Protection::READ.union(Protection::WRITE)),
                Err(MemoryError::Unmapped),
            );
        }
        ledger.validate().unwrap();
    }
}

#[test]
fn anywhere_reports_exhaustion() {
    let ledger = MemoryLedger::new();
    ledger.map(request(0x1000, PAGE * 3, 0, Protection::READ)).unwrap();
    let before = ledger.regions();
    let mut mapping = anonymous_request(None, 4);
    mapping.placement = Placement::Anywhere {
        minimum: address(0x1000),
        maximum: address(0x4000),
        hint: None,
    };
    assert_eq!(ledger.map(mapping), Err(MemoryError::NoAddressSpace));
    assert_eq!(ledger.regions(), before);
}

struct PageModel {
    pages: BTreeMap<usize, Protection>,
    random: u64,
}

impl PageModel {
    fn new() -> Self {
        Self {
            pages: BTreeMap::new(),
            random: 0x9e37_79b9,
        }
    }

    fn next(&mut self) -> (usize, usize, u64, Protection) {
        self.random = self
            .random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let first = (self.random as usize >> 8) % 32;
        let count = (((self.random as usize >> 16) % 4) + 1).min(32 - first);
        let protection = if self.random & 0x80_0000 == 0 {
            Protection::READ
        } else {
            Protection::READ.union(Protection::WRITE)
        };
        (first, count, self.random % 3, protection)
    }

    fn apply(&mut self, ledger: &MemoryLedger, first: usize, count: usize, operation: u64, protection: Protection) {
        let start = 0x20_000 + first as u64 * PAGE;
        let affected = range(start, count as u64 * PAGE);
        match operation {
            0 => self.map_pages(ledger, first, count, start, protection),
            1 => self.unmap_pages(ledger, first, count, affected),
            _ => self.protect_pages(ledger, first, count, affected, protection),
        }
    }

    fn map_pages(&mut self, ledger: &MemoryLedger, first: usize, count: usize, start: u64, protection: Protection) {
        ledger
            .map(request(start, count as u64 * PAGE, first as u64 * PAGE, protection))
            .unwrap();
        for page in first..first + count {
            self.pages.insert(page, protection);
        }
    }

    fn unmap_pages(&mut self, ledger: &MemoryLedger, first: usize, count: usize, affected: AddressRange) {
        ledger.unmap(affected).unwrap();
        for page in first..first + count {
            self.pages.remove(&page);
        }
    }

    fn protect_pages(
        &mut self,
        ledger: &MemoryLedger,
        first: usize,
        count: usize,
        affected: AddressRange,
        protection: Protection,
    ) {
        if (first..first + count).all(|page| self.pages.contains_key(&page)) {
            ledger.protect(affected, protection).unwrap();
            for page in first..first + count {
                *self.pages.get_mut(&page).unwrap() = protection;
            }
        } else {
            assert_eq!(
                ledger.protect(affected, protection),
                Err(MemoryError::Unmapped),
            );
        }
    }

    fn assert_matches(&self, ledger: &MemoryLedger) {
        ledger.validate().unwrap();
        for page in 0..32 {
            let observed = ledger
                .resolve(address(0x20_000 + page as u64 * PAGE), Protection::NONE)
                .map(|resolution| resolution.region.protection());
            assert_eq!(observed, self.pages.get(&page).copied());
        }
    }
}

#[test]
fn deterministic_page_model() {
    let ledger = MemoryLedger::new();
    let mut model = PageModel::new();
    for _ in 0..1_000 {
        let (first, count, operation, protection) = model.next();
        model.apply(&ledger, first, count, operation, protection);
        model.assert_matches(&ledger);
    }
}
