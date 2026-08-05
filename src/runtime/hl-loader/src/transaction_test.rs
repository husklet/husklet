use hl_isa::GuestArchitecture;
use std::sync::{Arc, Mutex};

use super::*;
use crate::test_support::{
    FakeAddressSpace, FakeSource, LINK_BASE, SegmentFixture, TransactionFixture, Transcript, fixture_with_tls,
    put_program_header, put_u16,
};

fn with_pure_bss(mut image: Vec<u8>) -> Vec<u8> {
    put_u16(&mut image, 56, 3);
    put_program_header(
        &mut image,
        2,
        SegmentFixture {
            kind: 1,
            flags: 6,
            offset: 0xffe8,
            address: LINK_BASE + 0x1ffe8,
            file_size: 0,
            memory_size: 0x1800,
            alignment: 0x10000,
        },
    );
    image
}

#[derive(Default)]
struct Diagnostics(Mutex<Vec<LoaderDiagnostic>>);

impl LoaderDiagnostics for Diagnostics {
    fn try_publish(&self, diagnostic: LoaderDiagnostic) {
        self.0.lock().unwrap().push(diagnostic);
    }
}

#[test]
fn diagnostics_cover_phases() {
    let diagnostics = Arc::new(Diagnostics::default());
    let mut loader = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, None)
        .with_diagnostics(diagnostics.clone());
    loader
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    let phases = diagnostics
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic.phase)
        .collect::<Vec<_>>();
    assert_eq!(
        phases,
        vec![
            LoaderPhase::MainRead,
            LoaderPhase::MainInspect,
            LoaderPhase::InterpreterPrepare,
            LoaderPhase::MainStage,
            LoaderPhase::InterpreterStage,
            LoaderPhase::StackPlan,
            LoaderPhase::Commit,
        ]
    );
}

#[test]
fn aarch64_fixed_executable() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, None);
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(loaded.main().address(), LINK_BASE);
    assert_eq!(loaded.interpreter().unwrap().address(), 0x60_0000);
    assert_eq!(loaded.stack_mapping().address(), 0x70_0000);
    assert_eq!(loaded.stack_mapping().size(), 0x120_000);
    assert_eq!(loaded.usable_stack().address(), 0x80_0000);
    assert_eq!(loaded.usable_stack().size(), 0x20_000);
    assert_eq!(loaded.stack_overread(), None);
    assert!(loaded.initial_stack().stack_pointer() >= 0x80_0000);

    let (source, address_space) = loader.into_parts();
    assert_eq!(source.transcript.len(), 2);
    assert!(address_space.published);
    assert!(matches!(
        address_space.transcript.first(),
        Some(Transcript::Reserve(
            1,
            MappingKind::MainImage,
            _,
            MappingPlacement::Fixed(LINK_BASE)
        ))
    ));
    assert_eq!(
        address_space.transcript.last(),
        Some(&Transcript::Commit(vec![1, 2, 3]))
    );
    assert!(
        !address_space
            .transcript
            .iter()
            .any(|event| matches!(event, Transcript::Rollback(_)))
    );
}

#[test]
fn stack_guard_bounds() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, None);
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(loaded.stack_mapping().address(), 0x70_0000);
    assert_eq!(loaded.stack_mapping().size(), 0x120_000);
    assert_eq!(loaded.usable_stack().address(), 0x80_0000);
    assert_eq!(loaded.usable_stack().size(), 0x20_000);
    let (_, address_space) = loader.into_parts();

    assert!(address_space.transcript.contains(&Transcript::Reserve(
        3,
        MappingKind::Stack,
        0x120_000,
        MappingPlacement::Hint(Some(0x70_0000)),
    )));
    assert!(
        address_space
            .transcript
            .contains(&Transcript::Protect(3, 0, 0x100_000, 0))
    );
    assert!(
        address_space
            .transcript
            .contains(&Transcript::Protect(3, 0x100_000, 0x20_000, 6))
    );
}

#[test]
fn stack_overread_bounds() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::X86_64, ImageKind::Executable, None);
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(loaded.stack_mapping().address(), 0x70_0000);
    assert_eq!(loaded.stack_mapping().size(), 0x130_000);
    assert_eq!(loaded.usable_stack().address(), 0x80_0000);
    assert_eq!(loaded.usable_stack().size(), 0x20_000);
    let overread = loaded.stack_overread().unwrap();
    assert_eq!(overread.address(), 0x82_0000);
    assert_eq!(overread.size(), 0x10_000);
    assert!(loaded.initial_stack().stack_pointer() < 0x82_0000);
    let (_, address_space) = loader.into_parts();
    assert!(address_space.transcript.contains(&Transcript::Reserve(
        3,
        MappingKind::Stack,
        0x130_000,
        MappingPlacement::Hint(Some(0x70_0000)),
    )));
    assert!(
        address_space
            .transcript
            .contains(&Transcript::Protect(3, 0x100_000, 0x30_000, 6))
    );
}

#[test]
fn bss_stages_zero() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut source = FakeSource::new(architecture, ImageKind::Executable);
        source.main = with_pure_bss(source.main);
        let mut loader = Loader::new(source, FakeAddressSpace::new(None), TransactionFixture::limits());

        loader.load(TransactionFixture::request(architecture)).unwrap();
        let (_, address_space) = loader.into_parts();

        assert!(
            address_space
                .transcript
                .iter()
                .any(|event| { matches!(event, Transcript::Zero(1, 0x1ffe8, 0x1800)) })
        );
        assert!(
            !address_space
                .transcript
                .iter()
                .any(|event| { matches!(event, Transcript::Write(1, 0x1ffe8, 0)) })
        );
    }
}

#[test]
fn x86_pie_uses() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::X86_64, ImageKind::PositionIndependent, None);
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(loaded.main().address(), 0x90_0000);
    let entry = loaded
        .initial_stack()
        .auxiliary()
        .iter()
        .find(|entry| entry.kind() == AuxiliaryType::Entry)
        .unwrap();
    assert_eq!(entry.value(), 0x90_0180);
    let (_, address_space) = loader.into_parts();
    assert!(matches!(
        address_space.transcript.first(),
        Some(Transcript::Reserve(
            1,
            MappingKind::MainImage,
            _,
            MappingPlacement::Hint(Some(0x90_0000))
        ))
    ));
}

#[test]
fn tls_and_dynamic() {
    let mut source = FakeSource::new(GuestArchitecture::X86_64, ImageKind::PositionIndependent);
    source.main = fixture_with_tls(
        GuestArchitecture::X86_64,
        ImageKind::PositionIndependent,
        true,
        &[1, 2],
        32,
        64,
    );
    source.interpreter = fixture_with_tls(
        GuestArchitecture::X86_64,
        ImageKind::PositionIndependent,
        false,
        &[3, 4, 5],
        48,
        32,
    );
    let mut loader = Loader::new(source, FakeAddressSpace::new(None), TransactionFixture::limits());
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    let tls = loaded.initial_tls();
    assert_eq!(tls.modules().len(), 2);
    assert_eq!(tls.modules()[0].role(), ImageRole::Main);
    assert_eq!(tls.modules()[0].module_id(), 1);
    assert_eq!(tls.modules()[0].template().zero_fill_size(), 30);
    assert_eq!(tls.modules()[1].role(), ImageRole::Interpreter);
    assert_eq!(tls.modules()[1].module_id(), 2);
    assert_eq!(tls.modules()[1].template().zero_fill_size(), 45);

    let handoff = loaded.dynamic_handoff();
    assert_eq!(handoff.modules().len(), 2);
    assert_eq!(handoff.modules()[0].tls_module_id, Some(1));
    assert_eq!(handoff.modules()[1].tls_module_id, Some(2));
    assert_eq!(handoff.main_entry(), 0x90_0180);
    assert_eq!(handoff.interpreter_base(), 0x20_0000);
    assert_eq!(handoff.start_entry(), 0x60_0180);
    let interpreter_base = loaded
        .initial_stack()
        .auxiliary()
        .iter()
        .find(|entry| entry.kind() == AuxiliaryType::InterpreterBase)
        .unwrap();
    assert_eq!(interpreter_base.value(), handoff.interpreter_base());

    let (_, address_space) = loader.into_parts();
    let reservations = address_space
        .transcript
        .iter()
        .filter(|event| matches!(event, Transcript::Reserve(..)))
        .count();
    assert_eq!(reservations, 3);
    assert_eq!(
        address_space.transcript.last(),
        Some(&Transcript::Commit(vec![1, 2, 3]))
    );
}

#[test]
fn explicit_rebased_executable() {
    let mut limits = TransactionFixture::limits();
    limits.executable_placement = ExecutablePlacement::Rebased {
        deterministic_hint: Some(0xa0_0000),
    };
    let mut loader = Loader::new(
        FakeSource::new(GuestArchitecture::Aarch64, ImageKind::Executable),
        FakeAddressSpace::new(None),
        limits,
    );
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(loaded.main().address(), 0xa0_0000);
    let entry = loaded
        .initial_stack()
        .auxiliary()
        .iter()
        .find(|entry| entry.kind() == AuxiliaryType::Entry)
        .unwrap();
    assert_eq!(entry.value(), LINK_BASE + 0x180);
}

#[test]
fn collision_fallback() {
    let mut limits = TransactionFixture::limits();
    limits.executable_placement = ExecutablePlacement::PreferLink {
        fallback_hint: Some(0xa0_0000),
    };
    let source = FakeSource::new(GuestArchitecture::X86_64, ImageKind::Executable);
    let address_space = FakeAddressSpace::new(None).conflict_fixed();
    let mut loader = Loader::new(source, address_space, limits);
    let loaded = loader
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(loaded.main().address(), 0xa0_0000);
    let projection = loaded.main_projection().unwrap();
    assert_eq!(projection.guest.start, LINK_BASE);
    assert_eq!(projection.storage_bias, 0xa0_0000 - LINK_BASE);
    assert_eq!(projection.storage_address(LINK_BASE + 8), Some(0xa0_0008));
    assert_eq!(projection.guest_address(0xa0_0008), Some(LINK_BASE + 8));
    assert_eq!(projection.storage_address(0x20_0000), Some(0x20_0000));
}

#[test]
fn failure_no_fallback() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::X86_64, ImageKind::Executable, Some(1));
    assert_eq!(
        loader.load(TransactionFixture::request(GuestArchitecture::X86_64)),
        Err(LoadError::AddressSpace(AddressSpaceError::Unavailable)),
    );
    let (_, address_space) = loader.into_parts();
    assert!(address_space.transcript.is_empty());
}

#[test]
fn every_address_space() {
    let mut successful = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, None);
    successful
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    let (_, successful_address_space) = successful.into_parts();
    let successful_operation_count = successful_address_space.operation;
    for failure in 1..=successful_operation_count {
        let mut loader = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, Some(failure));
        assert!(
            loader
                .load(TransactionFixture::request(GuestArchitecture::Aarch64))
                .is_err()
        );
        let (_, address_space) = loader.into_parts();
        assert!(!address_space.published, "failure point {failure}");
        assert!(address_space.reservations.is_empty(), "failure point {failure}");
        let rollbacks = TransactionFixture::rollback_tokens(&address_space.transcript);
        let expected = TransactionFixture::reserved_tokens_in(&address_space.transcript);
        assert_eq!(rollbacks, expected, "failure point {failure}");
    }
}

#[test]
fn source_failures_never() {
    for role in [ImageRole::Main, ImageRole::Interpreter] {
        let mut source = FakeSource::new(GuestArchitecture::X86_64, ImageKind::Executable);
        source.fail_role = Some(role);
        let mut loader = Loader::new(source, FakeAddressSpace::new(None), TransactionFixture::limits());
        assert_eq!(
            loader.load(TransactionFixture::request(GuestArchitecture::X86_64)),
            Err(LoadError::Source {
                role,
                error: ImageSourceError::Io
            })
        );
        let (_, address_space) = loader.into_parts();
        assert!(address_space.transcript.is_empty());
        assert!(!address_space.published);
    }
}
