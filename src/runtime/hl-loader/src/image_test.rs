//! Generic ELF placement and projection regressions.

use hl_isa::GuestArchitecture;

use super::*;
use crate::test_support::{LINK_BASE, fixture};

#[test]
fn markers_irrelevant() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let plain = fixture(architecture, ImageKind::Executable, false);
        let mut marked = plain.clone();
        marked[0x260..0x26e].copy_from_slice(b"\xff Go buildinf:");
        marked[0x300..0x309].copy_from_slice(b"gopclntab");
        marked[0x340..0x348].copy_from_slice(b"v8_blob_");
        let inspector = ElfInspector::new(architecture, ImageLimits::default());
        assert_eq!(inspector.inspect(&marked), inspector.inspect(&plain));
    }
}

#[test]
fn displaced_projection() {
    use crate::test_support::{FakeAddressSpace, FakeSource, TransactionFixture};

    let mut limits = TransactionFixture::limits();
    limits.executable_placement = ExecutablePlacement::Rebased {
        deterministic_hint: Some(0xa0_0000),
    };
    let mut loader = Loader::new(
        FakeSource::new(GuestArchitecture::X86_64, ImageKind::Executable),
        FakeAddressSpace::new(None),
        limits,
    );
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    let projection = loaded.main_projection().unwrap();
    assert_eq!(projection.guest.start, LINK_BASE);
    assert_eq!(projection.storage_bias, 0xa0_0000 - LINK_BASE);
    assert_eq!(projection.storage_address(LINK_BASE + 8), Some(0xa0_0008));
    assert_eq!(projection.guest_address(0xa0_0008), Some(LINK_BASE + 8));
    assert_eq!(projection.storage_address(0x20_0000), Some(0x20_0000));
}

#[test]
fn native_placement() {
    use crate::test_support::{FakeAddressSpace, FakeSource, TransactionFixture};

    for kind in [ImageKind::Executable, ImageKind::PositionIndependent] {
        let mut loader = Loader::new(
            FakeSource::new(GuestArchitecture::Aarch64, kind),
            FakeAddressSpace::new(None),
            TransactionFixture::limits(),
        );
        let loaded = loader
            .load(TransactionFixture::request(GuestArchitecture::Aarch64))
            .unwrap();
        assert_eq!(loaded.main_projection(), None);
    }
}
