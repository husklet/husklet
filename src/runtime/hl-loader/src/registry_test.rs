use hl_isa::GuestArchitecture;

use crate::test_support::{FakeAddressSpace, FakeSource, TransactionFixture, Transcript};
use crate::{AddressSpaceError, ExecutablePlacement, ImageKind, ImageProtectionRegistry};

impl ImageProtectionRegistry<u32> for FakeAddressSpace {
    fn stage_executable(&mut self, reservation: &u32, mapping_offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.validate_range(*reservation, mapping_offset, size)?;
        self.transcript
            .push(Transcript::Executable(*reservation, mapping_offset, size));
        Ok(())
    }

    fn stage_guest_access(
        &mut self,
        reservation: &u32,
        guest_address: u64,
        size: u64,
        read_only: bool,
    ) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.transcript
            .push(Transcript::GuestAccess(*reservation, guest_address, size, read_only));
        Ok(())
    }
}

#[test]
fn executable_and_guest() {
    let mut loader = TransactionFixture::loader(GuestArchitecture::Aarch64, ImageKind::Executable, None);
    loader
        .load(TransactionFixture::request(GuestArchitecture::Aarch64))
        .unwrap();
    let (_, address_space) = loader.into_parts();
    let executable = address_space
        .transcript
        .iter()
        .position(|event| matches!(event, Transcript::Executable(1, 0, 0x1_0000)))
        .unwrap();
    let first_write = address_space
        .transcript
        .iter()
        .position(|event| matches!(event, Transcript::Write(1, ..)))
        .unwrap();
    assert!(executable < first_write);
    let text_protect = address_space
        .transcript
        .iter()
        .position(|event| matches!(event, Transcript::Protect(1, 0, 0x1000, _)))
        .unwrap();
    let text_registry = address_space
        .transcript
        .iter()
        .position(|event| matches!(event, Transcript::GuestAccess(1, 0x40_0000, 0x1000, true)))
        .unwrap();
    let data_registry = address_space
        .transcript
        .iter()
        .position(|event| matches!(event, Transcript::GuestAccess(1, 0x40_2000, 0x1000, false)))
        .unwrap();
    let commit = address_space.transcript.len() - 1;
    assert!(text_protect < text_registry);
    assert!(text_registry < commit && data_registry < commit);
    assert!(matches!(address_space.transcript[commit], Transcript::Commit(_)));
}

#[test]
fn registry_uses_low() {
    let mut executable_limits = TransactionFixture::limits();
    executable_limits.executable_placement = ExecutablePlacement::Rebased {
        deterministic_hint: Some(0xa0_0000),
    };
    let mut executable = crate::Loader::new(
        FakeSource::new(GuestArchitecture::X86_64, ImageKind::Executable),
        FakeAddressSpace::new(None),
        executable_limits,
    );
    executable
        .load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    let (_, executable) = executable.into_parts();
    assert!(
        executable
            .transcript
            .iter()
            .any(|event| { matches!(event, Transcript::GuestAccess(1, 0x40_0000, 0x1000, true)) })
    );

    let mut pie = TransactionFixture::loader(GuestArchitecture::X86_64, ImageKind::PositionIndependent, None);
    pie.load(TransactionFixture::request(GuestArchitecture::X86_64))
        .unwrap();
    let (_, pie) = pie.into_parts();
    assert!(
        pie.transcript
            .iter()
            .any(|event| { matches!(event, Transcript::GuestAccess(1, 0x90_0000, 0x1000, true)) })
    );
}
