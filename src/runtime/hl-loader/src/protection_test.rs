use hl_isa::GuestArchitecture;

use super::*;
use crate::test_support::{LINK_BASE, SegmentFixture, fixture, put_program_header, put_u16};

struct RelroFixture;

impl RelroFixture {
    fn image(architecture: GuestArchitecture) -> Vec<u8> {
        let mut bytes = fixture(architecture, ImageKind::PositionIndependent, false);
        bytes[64 + 56 + 40..64 + 56 + 48].copy_from_slice(&0x1000_u64.to_le_bytes());
        put_u16(&mut bytes, 56, 3);
        put_program_header(
            &mut bytes,
            2,
            SegmentFixture {
                kind: 0x6474_e552,
                flags: 4,
                offset: 0,
                address: LINK_BASE + 0x2000,
                file_size: 0,
                memory_size: 0x1000,
                alignment: 1,
            },
        );
        bytes
    }
}

#[test]
fn relro_is_validated() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let bytes = RelroFixture::image(architecture);
        let image = ElfInspector::new(architecture, ImageLimits::default())
            .inspect(&bytes)
            .unwrap();
        assert_eq!(image.relro(), Some(RelroRegion::new(LINK_BASE + 0x2000, 0x1000)));
        let plan = ImageProtectionPlan::build(&image, 4096).unwrap();
        let relro_page = plan
            .ranges()
            .iter()
            .find(|range| range.mapping_offset() == 0x2000)
            .unwrap();
        assert_ne!(relro_page.protection().bits() & Protection::WRITE, 0);
    }
}

#[test]
fn relro_initial_write() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut bytes = RelroFixture::image(architecture);
        let base = 64 + 2 * 56;
        bytes[base + 16..base + 24].copy_from_slice(&(LINK_BASE + 0x2108).to_le_bytes());
        bytes[base + 40..base + 48].copy_from_slice(&0x0ef8_u64.to_le_bytes());
        let image = ElfInspector::new(architecture, ImageLimits::default())
            .inspect(&bytes)
            .unwrap();
        let plan = ImageProtectionPlan::build(&image, 4096).unwrap();
        let relro_page = plan
            .ranges()
            .iter()
            .find(|range| range.mapping_offset() == 0x2000)
            .unwrap();
        assert_eq!(relro_page.size(), 0x1000);
        assert_ne!(relro_page.protection().bits() & Protection::WRITE, 0);
    }
}

#[test]
fn relro_may_include_the_final_mapped_page_padding() {
    let mut bytes = RelroFixture::image(GuestArchitecture::Aarch64);
    put_program_header(
        &mut bytes,
        1,
        SegmentFixture {
            kind: 1,
            flags: 6,
            offset: 0x0aa0,
            address: LINK_BASE + 0x2aa0,
            file_size: 0x0660,
            memory_size: 0x0660,
            alignment: 0x1000,
        },
    );
    put_program_header(
        &mut bytes,
        2,
        SegmentFixture {
            kind: 0x6474_e552,
            flags: 4,
            offset: 0x0aa0,
            address: LINK_BASE + 0x2aa0,
            file_size: 0x0660,
            memory_size: 0x1560,
            alignment: 1,
        },
    );

    let image = ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
        .inspect(&bytes)
        .unwrap();

    assert_eq!(image.relro(), Some(RelroRegion::new(LINK_BASE + 0x2aa0, 0x1560)));
}

#[test]
fn malformed_relro_is() {
    let original = RelroFixture::image(GuestArchitecture::X86_64);
    for (address, size, expected) in [
        (LINK_BASE + 0x5000, 0x100_u64, InspectError::InvalidRelro),
        (LINK_BASE + 0x2000, 0_u64, InspectError::InvalidRelro),
        (u64::MAX - 1, 4_u64, InspectError::InvalidRelro),
    ] {
        let mut bytes = original.clone();
        let base = 64 + 2 * 56;
        bytes[base + 16..base + 24].copy_from_slice(&address.to_le_bytes());
        bytes[base + 40..base + 48].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            ElfInspector::new(GuestArchitecture::X86_64, ImageLimits::default()).inspect(&bytes),
            Err(expected)
        );
    }
}

#[test]
fn host_page_union() {
    let bytes = fixture(GuestArchitecture::Aarch64, ImageKind::PositionIndependent, false);
    let image = ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
        .inspect(&bytes)
        .unwrap();
    assert_eq!(
        ImageProtectionPlan::build(&image, 0),
        Err(ProtectionPlanError::InvalidHostPageSize)
    );
    assert_eq!(
        ImageProtectionPlan::build(&image, 16_384),
        Err(ProtectionPlanError::WriteExecutePage)
    );
}

#[test]
fn guest_registry_ranges() {
    let bytes = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    let image = ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
        .inspect(&bytes)
        .unwrap();
    let plan = GuestProtectionPlan::build(&image, LINK_BASE).unwrap();
    assert_eq!(
        plan.ranges(),
        &[
            GuestProtectionRange {
                guest_address: LINK_BASE,
                size: 0x1000,
                read_only: true,
            },
            GuestProtectionRange {
                guest_address: LINK_BASE + 0x2000,
                size: 0x1000,
                read_only: false,
            },
        ]
    );
}
