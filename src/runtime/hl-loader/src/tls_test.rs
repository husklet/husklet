use hl_isa::GuestArchitecture;

use super::*;
use crate::test_support::{LINK_BASE, SegmentFixture, fixture_with_tls, put_program_header};

#[test]
fn parser_retains_initialized() {
    let bytes = fixture_with_tls(
        GuestArchitecture::Aarch64,
        ImageKind::Executable,
        false,
        &[1, 2, 3, 4],
        32,
        64,
    );
    let plan = ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
        .inspect(&bytes)
        .unwrap();
    let tls = plan.tls().unwrap();
    assert_eq!(tls.link_address(), LINK_BASE + 0x2040);
    assert_eq!(tls.initialized(), &[1, 2, 3, 4]);
    assert_eq!(tls.memory_size(), 32);
    assert_eq!(tls.zero_fill_size(), 28);
    assert_eq!(tls.alignment(), 64);
}

#[test]
fn parser_rejects_malformed() {
    let original = fixture_with_tls(
        GuestArchitecture::X86_64,
        ImageKind::Executable,
        false,
        &[1, 2, 3, 4],
        8,
        64,
    );
    for (field_offset, value, expected) in [
        (64 + 2 * 56 + 40, 3, InspectError::TlsFileLargerThanMemory),
        (64 + 2 * 56 + 48, 3, InspectError::InvalidTlsAlignment),
        (64 + 2 * 56 + 16, LINK_BASE + 0x2041, InspectError::InvalidTlsAlignment),
    ] {
        let mut bytes = original.clone();
        bytes[field_offset..field_offset + 8].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            ElfInspector::new(GuestArchitecture::X86_64, ImageLimits::default()).inspect(&bytes),
            Err(expected)
        );
    }
    let mut outside = original.clone();
    let outside_offset = outside.len() as u64;
    outside[64 + 2 * 56 + 8..64 + 2 * 56 + 16].copy_from_slice(&outside_offset.to_le_bytes());
    assert_eq!(
        ElfInspector::new(GuestArchitecture::X86_64, ImageLimits::default()).inspect(&outside),
        Err(InspectError::TlsOutsideImage)
    );
}

#[test]
fn parser_rejects_multiple() {
    let mut bytes = fixture_with_tls(GuestArchitecture::Aarch64, ImageKind::Executable, false, &[], 16, 16);
    crate::test_support::put_u16(&mut bytes, 56, 4);
    put_program_header(
        &mut bytes,
        3,
        SegmentFixture {
            kind: 7,
            flags: 4,
            offset: 0x1040,
            address: LINK_BASE + 0x2040,
            file_size: 0,
            memory_size: 16,
            alignment: 16,
        },
    );
    assert_eq!(
        ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default()).inspect(&bytes),
        Err(InspectError::MultipleTlsSegments)
    );
}

#[test]
fn both_isa_plans() {
    let template = TlsTemplate::new(LINK_BASE, vec![9, 8], 32, 64);
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let plan = InitialTlsPlan::build(
            architecture,
            &[TlsModuleRequest {
                role: ImageRole::Main,
                load_bias: 0x1000,
                template: &template,
            }],
        )
        .unwrap();
        assert_eq!(plan.architecture(), architecture);
        assert_eq!(plan.allocation_alignment(), 64);
        assert_eq!(plan.modules()[0].storage().start % 64, 0);
        assert_eq!(plan.modules()[0].template().zero_fill_size(), 30);
        assert!(plan.dtv().end <= plan.allocation_size());
        match architecture {
            GuestArchitecture::Aarch64 => {
                assert_eq!(plan.variant(), TlsVariant::VariantOne);
                assert!(plan.tcb().end <= plan.modules()[0].storage().start);
            }
            GuestArchitecture::X86_64 => {
                assert_eq!(plan.variant(), TlsVariant::VariantTwo);
                assert!(plan.modules()[0].storage().end <= plan.tcb().start);
            }
        }
    }
}

#[test]
fn multi_module_plan() {
    let main = TlsTemplate::new(0x100, vec![1], 8, 8);
    let interpreter = TlsTemplate::new(0x200, vec![2], 16, 16);
    let plan = InitialTlsPlan::build(
        GuestArchitecture::X86_64,
        &[
            TlsModuleRequest {
                role: ImageRole::Main,
                load_bias: 0x1000,
                template: &main,
            },
            TlsModuleRequest {
                role: ImageRole::Interpreter,
                load_bias: 0x2000,
                template: &interpreter,
            },
        ],
    )
    .unwrap();
    assert_eq!(plan.modules()[0].module_id(), 1);
    assert_eq!(plan.modules()[0].role(), ImageRole::Main);
    assert_eq!(plan.modules()[0].runtime_image_address(), 0x1100);
    assert_eq!(plan.modules()[1].module_id(), 2);
    assert_eq!(plan.modules()[1].role(), ImageRole::Interpreter);
    assert_eq!(plan.modules()[1].runtime_image_address(), 0x2200);
}
