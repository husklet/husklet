use hl_isa::GuestArchitecture;

use super::*;
use crate::test_support::{LINK_BASE, SegmentFixture, fixture, put_program_header, put_u16};

struct DynamicFixture;

impl DynamicFixture {
    fn image(architecture: GuestArchitecture, entries: &[(i64, u64)]) -> Vec<u8> {
        let mut bytes = fixture(architecture, ImageKind::PositionIndependent, false);
        put_u16(&mut bytes, 56, 3);
        let offset = 0x1080;
        for (index, (tag, value)) in entries.iter().enumerate() {
            let entry = offset + index * 16;
            bytes[entry..entry + 8].copy_from_slice(&tag.to_le_bytes());
            bytes[entry + 8..entry + 16].copy_from_slice(&value.to_le_bytes());
        }
        put_program_header(
            &mut bytes,
            2,
            SegmentFixture {
                kind: 2,
                flags: 6,
                offset: offset as u64,
                address: LINK_BASE + 0x2080,
                file_size: (entries.len() * 16) as u64,
                memory_size: (entries.len() * 16) as u64,
                alignment: 8,
            },
        );
        bytes
    }
}

#[test]
fn both_isas_retain() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let bytes = DynamicFixture::image(architecture, &[(7, 0x1234), (8, 24), (0, 0)]);
        let image = ElfInspector::new(architecture, ImageLimits::default())
            .inspect(&bytes)
            .unwrap();
        let dynamic = image.dynamic().unwrap();
        assert_eq!(dynamic.link_address(), LINK_BASE + 0x2080);
        assert_eq!(dynamic.entries().len(), 2);
        assert!(dynamic.relocation_writes().is_empty());
    }
}

#[test]
fn malformed_dynamic_shape() {
    let architecture = GuestArchitecture::X86_64;
    let unterminated = DynamicFixture::image(architecture, &[(7, 0x1234)]);
    assert_eq!(
        ElfInspector::new(architecture, ImageLimits::default()).inspect(&unterminated),
        Err(InspectError::UnterminatedDynamicTable)
    );

    let duplicate = DynamicFixture::image(architecture, &[(7, 0x1234), (7, 0x5678), (0, 0)]);
    assert_eq!(
        ElfInspector::new(architecture, ImageLimits::default()).inspect(&duplicate),
        Err(InspectError::DuplicateDynamicTag)
    );

    let mut truncated = DynamicFixture::image(architecture, &[(0, 0)]);
    let header = 64 + 2 * 56;
    truncated[header + 32..header + 40].copy_from_slice(&17_u64.to_le_bytes());
    assert_eq!(
        ElfInspector::new(architecture, ImageLimits::default()).inspect(&truncated),
        Err(InspectError::InvalidDynamicTable)
    );
}
