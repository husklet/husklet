use hl_isa::GuestArchitecture;

use super::*;

const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const LOAD_OFFSET: usize = 0;
const DATA_OFFSET: usize = 0x1000;
const INTERPRETER_OFFSET: usize = 0x200;
pub(crate) const LINK_BASE: u64 = 0x40_0000;

#[derive(Clone, Copy)]
pub(crate) struct SegmentFixture {
    pub(crate) kind: u32,
    pub(crate) flags: u32,
    pub(crate) offset: u64,
    pub(crate) address: u64,
    pub(crate) file_size: u64,
    pub(crate) memory_size: u64,
    pub(crate) alignment: u64,
}

pub(crate) fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_program_header(bytes: &mut [u8], index: usize, fixture: SegmentFixture) {
    let base = HEADER_SIZE + index * PROGRAM_HEADER_SIZE;
    put_u32(bytes, base, fixture.kind);
    put_u32(bytes, base + 4, fixture.flags);
    put_u64(bytes, base + 8, fixture.offset);
    put_u64(bytes, base + 16, fixture.address);
    put_u64(bytes, base + 24, fixture.address);
    put_u64(bytes, base + 32, fixture.file_size);
    put_u64(bytes, base + 40, fixture.memory_size);
    put_u64(bytes, base + 48, fixture.alignment);
}

pub(crate) fn fixture(architecture: GuestArchitecture, kind: ImageKind, interpreter: bool) -> Vec<u8> {
    let header_count = if interpreter { 3 } else { 2 };
    let mut bytes = vec![0_u8; DATA_OFFSET + 0x100];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[7] = 0;
    put_u16(
        &mut bytes,
        16,
        match kind {
            ImageKind::Executable => 2,
            ImageKind::PositionIndependent => 3,
        },
    );
    put_u16(&mut bytes, 18, architecture.elf_machine());
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + 0x180);
    put_u64(&mut bytes, 32, HEADER_SIZE as u64);
    put_u16(&mut bytes, 52, HEADER_SIZE as u16);
    put_u16(&mut bytes, 54, PROGRAM_HEADER_SIZE as u16);
    put_u16(&mut bytes, 56, header_count);
    put_program_header(
        &mut bytes,
        0,
        SegmentFixture {
            kind: 1,
            flags: 5,
            offset: LOAD_OFFSET as u64,
            address: LINK_BASE,
            file_size: 0x300,
            memory_size: 0x300,
            alignment: 0x1000,
        },
    );
    put_program_header(
        &mut bytes,
        1,
        SegmentFixture {
            kind: 1,
            flags: 6,
            offset: DATA_OFFSET as u64,
            address: LINK_BASE + 0x2000,
            file_size: 0x100,
            memory_size: 0x280,
            alignment: 0x1000,
        },
    );
    if interpreter {
        const PATH: &[u8] = b"/lib/ld-linux.so.1\0";
        bytes[INTERPRETER_OFFSET..INTERPRETER_OFFSET + PATH.len()].copy_from_slice(PATH);
        put_program_header(
            &mut bytes,
            2,
            SegmentFixture {
                kind: 3,
                flags: 4,
                offset: INTERPRETER_OFFSET as u64,
                address: 0,
                file_size: PATH.len() as u64,
                memory_size: PATH.len() as u64,
                alignment: 1,
            },
        );
    }
    bytes
}

fn inspect(architecture: GuestArchitecture, image: &[u8]) -> Result<ImagePlan, InspectError> {
    ElfInspector::new(architecture, ImageLimits::default()).inspect(image)
}

#[test]
fn fixture_derived_executable() {
    let bytes = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, true);
    let plan = inspect(GuestArchitecture::Aarch64, &bytes).unwrap();
    assert_eq!(plan.architecture(), GuestArchitecture::Aarch64);
    assert_eq!(plan.kind(), ImageKind::Executable);
    assert_eq!(plan.entry(), LINK_BASE + 0x180);
    assert_eq!(plan.link_base(), LINK_BASE);
    assert_eq!(plan.image_span(), 0x1_0000);
    assert_eq!(plan.segments().len(), 2);
    assert!(plan.segments()[0].flags().is_executable());
    assert_eq!(plan.segments()[1].zero_fill_size(), 0x180);
    assert_eq!(plan.interpreter().unwrap().as_bytes(), b"/lib/ld-linux.so.1");
    assert_eq!(
        plan.program_headers().guest_address(),
        Some(LINK_BASE + HEADER_SIZE as u64)
    );
}

#[test]
fn both_guest_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let bytes = fixture(architecture, ImageKind::PositionIndependent, false);
        let plan = inspect(architecture, &bytes).unwrap();
        assert_eq!(plan.architecture(), architecture);
        assert_eq!(plan.kind(), ImageKind::PositionIndependent);
        assert_eq!(plan.interpreter(), None);
    }
}

#[test]
fn wrong_architecture_is() {
    let bytes = fixture(GuestArchitecture::X86_64, ImageKind::Executable, false);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &bytes),
        Err(InspectError::WrongArchitecture)
    );
}

#[test]
fn identity_and_header() {
    let original = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    for (offset, value, expected) in [
        (0, 0, InspectError::InvalidMagic),
        (4, 1, InspectError::UnsupportedClass),
        (5, 2, InspectError::UnsupportedByteOrder),
        (6, 2, InspectError::UnsupportedVersion),
        (7, 9, InspectError::UnsupportedAbi),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        assert_eq!(inspect(GuestArchitecture::Aarch64, &bytes), Err(expected));
    }
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &original[..63]),
        Err(InspectError::TruncatedHeader)
    );
}

#[test]
fn program_header_count() {
    let original = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    let mut absent = original.clone();
    put_u16(&mut absent, 56, 0);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &absent),
        Err(InspectError::MissingProgramHeaders)
    );

    let mut excessive = original.clone();
    put_u16(&mut excessive, 56, 257);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &excessive),
        Err(InspectError::TooManyProgramHeaders)
    );

    let mut truncated = original;
    let truncated_offset = truncated.len() as u64 - 32;
    put_u64(&mut truncated, 32, truncated_offset);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &truncated),
        Err(InspectError::TruncatedProgramHeaders)
    );
}

#[test]
fn segment_file_and() {
    let original = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    let first = HEADER_SIZE;

    let mut oversized_file = original.clone();
    put_u64(&mut oversized_file, first + 32, 0x301);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &oversized_file),
        Err(InspectError::SegmentFileLargerThanMemory)
    );

    let mut outside = original.clone();
    let outside_offset = outside.len() as u64;
    put_u64(&mut outside, first + 8, outside_offset);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &outside),
        Err(InspectError::SegmentOutsideImage)
    );

    let mut overflow = original;
    put_u64(&mut overflow, first + 16, u64::MAX - 0x100);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &overflow),
        Err(InspectError::AddressOverflow)
    );
}

#[test]
fn alignment_and_entry() {
    let original = fixture(GuestArchitecture::X86_64, ImageKind::Executable, false);
    let mut alignment = original.clone();
    put_u64(&mut alignment, HEADER_SIZE + 48, 24);
    assert_eq!(
        inspect(GuestArchitecture::X86_64, &alignment),
        Err(InspectError::InvalidSegmentAlignment)
    );

    let mut entry = original;
    put_u64(&mut entry, 24, LINK_BASE + 0x2000);
    assert_eq!(
        inspect(GuestArchitecture::X86_64, &entry),
        Err(InspectError::EntryOutsideExecutableSegment)
    );

    let mut instruction_alignment = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    put_u64(&mut instruction_alignment, 24, LINK_BASE + 0x181);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &instruction_alignment),
        Err(InspectError::MisalignedEntry)
    );
}

#[test]
fn explicit_program_header() {
    let mut bytes = fixture(GuestArchitecture::X86_64, ImageKind::Executable, false);
    put_u32(&mut bytes, HEADER_SIZE + PROGRAM_HEADER_SIZE, 6);
    assert_eq!(
        inspect(GuestArchitecture::X86_64, &bytes),
        Err(InspectError::InvalidProgramHeaderAddress)
    );
}

#[test]
fn interpreter_must_be() {
    let original = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, true);
    let interpreter = HEADER_SIZE + 2 * PROGRAM_HEADER_SIZE;

    let mut unterminated = original.clone();
    let path_size = b"/lib/ld-linux.so.1\0".len();
    unterminated[INTERPRETER_OFFSET + path_size - 1] = b'x';
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &unterminated),
        Err(InspectError::UnterminatedInterpreter)
    );

    let mut embedded = original.clone();
    embedded[INTERPRETER_OFFSET + 4] = 0;
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &embedded),
        Err(InspectError::EmbeddedInterpreterNul)
    );

    let mut duplicate = original;
    put_u16(&mut duplicate, 56, 4);
    put_program_header(
        &mut duplicate,
        3,
        SegmentFixture {
            kind: 3,
            flags: 4,
            offset: INTERPRETER_OFFSET as u64,
            address: 0,
            file_size: path_size as u64,
            memory_size: path_size as u64,
            alignment: 1,
        },
    );
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &duplicate),
        Err(InspectError::MultipleInterpreters)
    );

    let mut empty = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, true);
    put_u64(&mut empty, interpreter + 32, 0);
    assert_eq!(
        inspect(GuestArchitecture::Aarch64, &empty),
        Err(InspectError::EmptyInterpreter)
    );
}

#[test]
fn caller_supplied_image() {
    let bytes = fixture(GuestArchitecture::Aarch64, ImageKind::Executable, false);
    let limits = ImageLimits {
        max_image_bytes: bytes.len() - 1,
        ..ImageLimits::default()
    };
    assert_eq!(
        ElfInspector::new(GuestArchitecture::Aarch64, limits).inspect(&bytes),
        Err(InspectError::ImageTooLarge)
    );

    let limits = ImageLimits {
        max_load_segments: 1,
        ..ImageLimits::default()
    };
    assert_eq!(
        ElfInspector::new(GuestArchitecture::Aarch64, limits).inspect(&bytes),
        Err(InspectError::TooManyLoadSegments)
    );
}
