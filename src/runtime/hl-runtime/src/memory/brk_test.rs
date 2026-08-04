use super::*;
use crate::BrkSnapshot;

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
