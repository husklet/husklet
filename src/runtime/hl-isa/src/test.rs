use super::*;

#[test]
fn public_guest_and() {
    assert_eq!(u32::from(GuestArchitecture::Aarch64), 1);
    assert_eq!(u32::from(GuestArchitecture::X86_64), 2);
    assert_eq!(HostArchitecture::Aarch64 as u32, 1);
    assert_eq!(HostArchitecture::X86_64 as u32, 2);
    assert_eq!(GuestArchitecture::try_from(1), Ok(GuestArchitecture::Aarch64));
    assert_eq!(GuestArchitecture::try_from(2), Ok(GuestArchitecture::X86_64));
    assert_eq!(GuestArchitecture::try_from(0), Err(InvalidArchitecture(0)));
    assert_eq!(GuestArchitecture::try_from(3), Err(InvalidArchitecture(3)));
}

#[test]
fn elf_linux_and() {
    assert_eq!(GuestArchitecture::Aarch64.elf_machine(), 0xb7);
    assert_eq!(GuestArchitecture::X86_64.elf_machine(), 0x3e);
    assert_eq!(GuestArchitecture::Aarch64.linux_stat_size(), 128);
    assert_eq!(GuestArchitecture::X86_64.linux_stat_size(), 144);
    assert_eq!(GuestArchitecture::Aarch64.instruction_alignment(), 4);
    assert_eq!(GuestArchitecture::X86_64.instruction_alignment(), 1);
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        assert_eq!(architecture.endianness(), Endianness::Little);
        assert_eq!(architecture.word_bits(), 64);
    }
}

#[test]
fn complete_host_guest() {
    assert_eq!(SUPPORTED_PAIRS.len(), 4);
    assert!(SUPPORTED_PAIRS.contains(&ArchitecturePair::new(
        HostArchitecture::Aarch64,
        GuestArchitecture::Aarch64
    )));
    assert!(SUPPORTED_PAIRS.contains(&ArchitecturePair::new(
        HostArchitecture::Aarch64,
        GuestArchitecture::X86_64
    )));
    assert!(SUPPORTED_PAIRS.contains(&ArchitecturePair::new(
        HostArchitecture::X86_64,
        GuestArchitecture::Aarch64
    )));
    assert!(SUPPORTED_PAIRS.contains(&ArchitecturePair::new(
        HostArchitecture::X86_64,
        GuestArchitecture::X86_64
    )));
    assert!(SUPPORTED_PAIRS[0].is_same_architecture());
    assert!(!SUPPORTED_PAIRS[1].is_same_architecture());
}

#[test]
fn word_page_and() {
    assert_eq!(GuestWordSize::new(64), Ok(GuestWordSize::BITS_64));
    assert_eq!(GuestWordSize::BITS_64.bytes(), 8);
    assert_eq!(GuestWordSize::new(32), Err(GeometryError::UnsupportedWordSize(32)));
    assert_eq!(GuestPageSize::new(4096), Ok(GuestPageSize::LINUX));
    assert_eq!(
        GuestPageSize::new(16_384),
        Err(GeometryError::UnsupportedPageSize(16_384))
    );
    assert_eq!(GuestPageSize::LINUX.offset_mask(), 4095);

    let address = GuestAddress::new(0x12_345);
    assert_eq!(address.page_base(GuestPageSize::LINUX).get(), 0x12_000);
    assert_eq!(address.page(GuestPageSize::LINUX).number(), 0x12);
    assert_eq!(
        address.page(GuestPageSize::LINUX).address(GuestPageSize::LINUX),
        Ok(GuestAddress::new(0x12_000))
    );
    assert_eq!(
        GuestAddress::new(u64::MAX).checked_add(1),
        Err(GeometryError::AddressOverflow)
    );
    assert_eq!(
        GuestAddress::new(1).checked_offset_from(GuestAddress::new(2)),
        Err(GeometryError::AddressUnderflow)
    );
    assert_eq!(
        GuestAddress::try_from(u128::from(u64::MAX) + 1),
        Err(GeometryError::AddressOverflow)
    );
}

#[test]
fn ranges_preserve_half() {
    let range = AddressRange::nonempty(GuestAddress::new(0x4000), 0x2000).unwrap();
    assert_eq!(range.start(), GuestAddress::new(0x4000));
    assert_eq!(range.end(), GuestAddress::new(0x6000));
    assert_eq!(range.length(), 0x2000);
    assert!(range.is_page_aligned(GuestPageSize::LINUX));
    assert!(range.contains(GuestAddress::new(0x4000)));
    assert!(range.contains(GuestAddress::new(0x5fff)));
    assert!(!range.contains(GuestAddress::new(0x6000)));
    assert_eq!(
        AddressRange::nonempty(GuestAddress::ZERO, 0),
        Err(GeometryError::EmptyRange)
    );
    assert_eq!(
        AddressRange::new(GuestAddress::new(u64::MAX), 2),
        Err(GeometryError::AddressOverflow)
    );
}

#[test]
fn aarch64_core_layout() {
    let architecture = GuestArchitecture::Aarch64;
    assert_layout(architecture, CoreRegister::GeneralPurpose(0), 0, 8);
    assert_layout(architecture, CoreRegister::GeneralPurpose(30), 240, 8);
    assert_layout(architecture, CoreRegister::StackPointer, 248, 8);
    assert_layout(architecture, CoreRegister::ProgramCounter, 256, 8);
    assert_layout(architecture, CoreRegister::ThreadPointer, 264, 8);
    assert_layout(architecture, CoreRegister::Vector(0), 384, 16);
    assert_layout(architecture, CoreRegister::Vector(31), 880, 16);
    assert_layout(architecture, CoreRegister::Flags, 1024, 8);
    assert_eq!(architecture.register_layout(CoreRegister::GeneralPurpose(31)), None);
    assert_eq!(architecture.register_layout(CoreRegister::SecondaryThreadPointer), None);
}

#[test]
fn x86_64_core() {
    let architecture = GuestArchitecture::X86_64;
    assert_layout(architecture, CoreRegister::GeneralPurpose(0), 0, 8);
    assert_layout(architecture, CoreRegister::GeneralPurpose(15), 120, 8);
    assert_layout(architecture, CoreRegister::StackPointer, 32, 8);
    assert_layout(architecture, CoreRegister::ProgramCounter, 128, 8);
    assert_layout(architecture, CoreRegister::Flags, 136, 8);
    assert_layout(architecture, CoreRegister::ThreadPointer, 144, 8);
    assert_layout(architecture, CoreRegister::SecondaryThreadPointer, 152, 8);
    assert_layout(architecture, CoreRegister::Vector(0), 400, 16);
    assert_layout(architecture, CoreRegister::Vector(15), 640, 16);
    assert_eq!(architecture.register_layout(CoreRegister::GeneralPurpose(16)), None);
    assert_eq!(architecture.register_layout(CoreRegister::Vector(16)), None);
}

fn assert_layout(architecture: GuestArchitecture, register: CoreRegister, offset: u32, size: u16) {
    let layout = architecture.register_layout(register).unwrap();
    assert_eq!(layout.offset(), offset);
    assert_eq!(layout.size(), size);
}
