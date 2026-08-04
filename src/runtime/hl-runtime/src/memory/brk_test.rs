use super::*;
use crate::{AnonymousMemoryAccount, BrkSnapshot};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct Account {
    limit: u64,
    current: AtomicU64,
}

impl crate::AnonymousMemoryAccount for Account {
    fn reserve(&self, bytes: u64) -> bool {
        self.current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .is_ok()
    }

    fn refund(&self, bytes: u64) {
        let result = self
            .current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(bytes)
            });
        assert!(result.is_ok());
    }

    fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }
}

impl Fixture {
    fn runtime_with_brk(&self, architecture: GuestArchitecture) -> RuntimeMemorySyscalls<Mapping, Memory> {
        let brk = BrkRegion::new(
            self.coordinator.clone(),
            BrkSnapshot {
                lower: GuestAddress::new(0x10_000),
                current: GuestAddress::new(0x10_000),
                upper: GuestAddress::new(0x20_000),
                backing_identity: 99,
            },
        )
        .unwrap();
        self.runtime(architecture).with_brk(brk)
    }
}

#[test]
fn isas_page_transactions() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime_with_brk(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0; 6]),
            LinuxResult::Value(0x10_000),
        );
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x12_123, 0, 0, 0, 0, 0],),
            LinuxResult::Value(0x12_123),
        );
        for address in [0x10_000, 0x11_000, 0x12_000] {
            assert!(
                fixture
                    .coordinator
                    .ledger()
                    .resolve(GuestAddress::new(address), Protection::WRITE)
                    .is_some()
            );
        }
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x11_800, 0, 0, 0, 0, 0],),
            LinuxResult::Value(0x11_800),
        );
        assert!(
            fixture
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(0x11_000), Protection::WRITE)
                .is_some()
        );
        assert!(
            fixture
                .coordinator
                .ledger()
                .resolve(GuestAddress::new(0x12_000), Protection::NONE)
                .is_none()
        );
        assert_eq!(
            runtime.handle(Fixture::operation("mincore"), [0x10_000, 8192, 32, 0, 0, 0],),
            LinuxResult::Value(0),
        );
    }
}

#[test]
fn brk_unchanged_break() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let mut runtime = fixture.runtime_with_brk(architecture);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x20_001, 0, 0, 0, 0, 0],),
            LinuxResult::Value(0x10_000),
        );
        runtime.handle(Fixture::operation("mmap"), [0x11_000, 4096, 1, 0x22, u64::MAX, 0]);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x12_000, 0, 0, 0, 0, 0],),
            LinuxResult::Value(0x10_000),
        );

        runtime.handle(Fixture::operation("munmap"), [0x11_000, 4096, 0, 0, 0, 0]);
        fixture.mapping.0.lock().unwrap().fail_commit = true;
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x12_000, 0, 0, 0, 0, 0],),
            LinuxResult::Value(0x10_000),
        );
        assert!(fixture.coordinator.ledger().regions().is_empty());
    }
}

#[test]
fn byte_exact_limit_refund_fork_and_drop() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let account = Arc::new(Account {
            limit: 0x123,
            current: AtomicU64::new(0),
        });
        let brk = BrkRegion::new(
            fixture.coordinator.clone(),
            BrkSnapshot {
                lower: GuestAddress::new(0x10_000),
                current: GuestAddress::new(0x10_000),
                upper: GuestAddress::new(0x20_000),
                backing_identity: 99,
            },
        )
        .unwrap()
        .with_account(account.clone())
        .unwrap();
        let mut runtime = fixture.runtime(architecture).with_brk(brk);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x10_123, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_123)
        );
        assert_eq!(account.current(), 0x123);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x10_124, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_123)
        );
        assert_eq!(account.current(), 0x123);
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x10_011, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_011)
        );
        assert_eq!(account.current(), 0x11);

        let child_fixture = Fixture::new();
        let mut child = runtime
            .fork_clone(
                child_fixture.coordinator.clone(),
                child_fixture.descriptors.clone(),
                child_fixture.memory.clone(),
                2,
            )
            .unwrap();
        assert_eq!(
            account.current(),
            0x22,
            "Rust fork deep-copies and charges the child address space"
        );
        assert_eq!(
            child.handle(Fixture::operation("brk"), [0x10_021, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_021)
        );
        assert_eq!(account.current(), 0x32, "child growth extends its charged deep copy");
        assert_eq!(
            child.handle(Fixture::operation("brk"), [0x10_001, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_001)
        );
        assert_eq!(account.current(), 0x12, "child shrink retains one charged child byte");
        drop(child);
        assert_eq!(
            account.current(),
            0x11,
            "dropping the child refunds its deep-copy contribution"
        );
        drop(runtime);
        assert_eq!(
            account.current(),
            0,
            "address-space teardown refunds its exact owned growth"
        );
    }
}

#[test]
fn failed_mapping_refunds_reservation() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let fixture = Fixture::new();
        let account = Arc::new(Account {
            limit: 4096,
            current: AtomicU64::new(0),
        });
        let brk = BrkRegion::new(
            fixture.coordinator.clone(),
            BrkSnapshot {
                lower: GuestAddress::new(0x10_000),
                current: GuestAddress::new(0x10_000),
                upper: GuestAddress::new(0x20_000),
                backing_identity: 99,
            },
        )
        .unwrap()
        .with_account(account.clone())
        .unwrap();
        let mut runtime = fixture.runtime(architecture).with_brk(brk);
        fixture.mapping.0.lock().unwrap().fail_commit = true;
        assert_eq!(
            runtime.handle(Fixture::operation("brk"), [0x10_101, 0, 0, 0, 0, 0]),
            LinuxResult::Value(0x10_000)
        );
        assert_eq!(account.current(), 0);
    }
}

#[test]
fn exec_replacement_refunds_old_address_space() {
    let fixture = Fixture::new();
    let account = Arc::new(Account {
        limit: 4096,
        current: AtomicU64::new(0),
    });
    let snapshot = BrkSnapshot {
        lower: GuestAddress::new(0x10_000),
        current: GuestAddress::new(0x10_000),
        upper: GuestAddress::new(0x20_000),
        backing_identity: 99,
    };
    let old = BrkRegion::new(fixture.coordinator.clone(), snapshot)
        .unwrap()
        .with_account(account.clone())
        .unwrap();
    assert_eq!(old.set(0x10_101), 0x10_101);
    assert_eq!(account.current(), 0x101);
    let replacement_fixture = Fixture::new();
    let replacement = BrkRegion::new(replacement_fixture.coordinator.clone(), snapshot)
        .unwrap()
        .with_account(account.clone())
        .unwrap();
    drop(old);
    assert_eq!(
        account.current(),
        0,
        "exec retirement refunds the old address-space owner"
    );
    assert_eq!(replacement.set(0x10_022), 0x10_022);
    assert_eq!(account.current(), 0x22);
    drop(replacement);
    assert_eq!(account.current(), 0);
}
