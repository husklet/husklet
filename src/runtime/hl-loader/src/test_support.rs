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

const MAIN_PATH: &[u8] = b"/bin/program";
const INTERPRETER_PATH: &[u8] = b"/lib/ld-linux.so.1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Transcript {
    Source(ImageRole, Vec<u8>, usize),
    Reserve(u32, MappingKind, u64, MappingPlacement),
    Write(u32, u64, usize),
    Qword(u32, u64, [u8; 8]),
    Zero(u32, u64, u64),
    Protect(u32, u64, u64, u8),
    Executable(u32, u64, u64),
    GuestAccess(u32, u64, u64, bool),
    Commit(Vec<u32>),
    Rollback(u32),
}

pub(crate) struct FakeSource {
    pub(crate) main: Vec<u8>,
    pub(crate) interpreter: Vec<u8>,
    pub(crate) fail_role: Option<ImageRole>,
    pub(crate) transcript: Vec<Transcript>,
}

impl FakeSource {
    pub(crate) fn new(architecture: GuestArchitecture, kind: ImageKind) -> Self {
        Self {
            main: fixture(architecture, kind, true),
            interpreter: fixture(architecture, ImageKind::PositionIndependent, false),
            fail_role: None,
            transcript: Vec::new(),
        }
    }
}

impl ImageSource for FakeSource {
    fn read_image(&mut self, role: ImageRole, path: &[u8], max_bytes: usize) -> Result<Vec<u8>, ImageSourceError> {
        self.transcript.push(Transcript::Source(role, path.to_vec(), max_bytes));
        if self.fail_role == Some(role) {
            return Err(ImageSourceError::Io);
        }
        let expected = match role {
            ImageRole::Main => MAIN_PATH,
            ImageRole::Interpreter => INTERPRETER_PATH,
        };
        if path != expected {
            return Err(ImageSourceError::NotFound);
        }
        let bytes = match role {
            ImageRole::Main => &self.main,
            ImageRole::Interpreter => &self.interpreter,
        };
        if bytes.len() > max_bytes {
            return Err(ImageSourceError::TooLarge);
        }
        Ok(bytes.clone())
    }
}

pub(crate) struct FakeAddressSpace {
    next_token: u32,
    next_address: u64,
    pub(crate) reservations: std::collections::BTreeMap<u32, (u64, u64)>,
    pub(crate) operation: usize,
    fail_at: Option<usize>,
    pub(crate) published: bool,
    conflict_fixed: bool,
    pub(crate) transcript: Vec<Transcript>,
}

impl FakeAddressSpace {
    pub(crate) fn new(fail_at: Option<usize>) -> Self {
        Self {
            next_token: 1,
            next_address: 0x10_0000,
            reservations: std::collections::BTreeMap::new(),
            operation: 0,
            fail_at,
            published: false,
            conflict_fixed: false,
            transcript: Vec::new(),
        }
    }

    pub(crate) fn operation_result(&mut self) -> Result<(), AddressSpaceError> {
        self.operation += 1;
        if self.fail_at == Some(self.operation) {
            Err(AddressSpaceError::Unavailable)
        } else {
            Ok(())
        }
    }

    pub(crate) fn conflict_fixed(mut self) -> Self {
        self.conflict_fixed = true;
        self
    }

    pub(crate) fn validate_range(&self, token: u32, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        let (_, mapping_size) = self.reservations.get(&token).ok_or(AddressSpaceError::InvalidRange)?;
        if offset.checked_add(size).is_none_or(|end| end > *mapping_size) {
            return Err(AddressSpaceError::InvalidRange);
        }
        Ok(())
    }
}

impl TransactionalAddressSpace for FakeAddressSpace {
    type Reservation = u32;

    fn reserve(
        &mut self,
        kind: MappingKind,
        size: u64,
        placement: MappingPlacement,
    ) -> Result<ReservedMapping<Self::Reservation>, AddressSpaceError> {
        self.operation_result()?;
        if self.conflict_fixed && matches!(placement, MappingPlacement::Fixed(_)) {
            return Err(AddressSpaceError::Conflict);
        }
        let address = match placement {
            MappingPlacement::Fixed(address) => address,
            MappingPlacement::Hint(Some(address)) => address,
            MappingPlacement::Hint(None) => {
                let address = self.next_address;
                self.next_address += 0x20_0000;
                address
            }
        };
        let token = self.next_token;
        self.next_token += 1;
        self.reservations.insert(token, (address, size));
        self.transcript.push(Transcript::Reserve(token, kind, size, placement));
        Ok(ReservedMapping::new(token, address, size))
    }

    fn stage_write(
        &mut self,
        reservation: &Self::Reservation,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.validate_range(*reservation, offset, bytes.len() as u64)?;
        if let Ok(qword) = <[u8; 8]>::try_from(bytes) {
            self.transcript.push(Transcript::Qword(*reservation, offset, qword));
        } else {
            self.transcript
                .push(Transcript::Write(*reservation, offset, bytes.len()));
        }
        Ok(())
    }

    fn stage_zero(&mut self, reservation: &Self::Reservation, offset: u64, size: u64) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.validate_range(*reservation, offset, size)?;
        self.transcript.push(Transcript::Zero(*reservation, offset, size));
        Ok(())
    }

    fn stage_protection(
        &mut self,
        reservation: &Self::Reservation,
        offset: u64,
        size: u64,
        protection: Protection,
    ) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.validate_range(*reservation, offset, size)?;
        self.transcript
            .push(Transcript::Protect(*reservation, offset, size, protection.bits()));
        Ok(())
    }

    fn commit(&mut self, reservations: &[Self::Reservation]) -> Result<(), AddressSpaceError> {
        self.operation_result()?;
        self.transcript.push(Transcript::Commit(reservations.to_vec()));
        self.published = true;
        Ok(())
    }

    fn rollback(&mut self, reservation: &Self::Reservation) {
        self.reservations.remove(reservation);
        self.transcript.push(Transcript::Rollback(*reservation));
    }
}

pub(crate) struct TransactionFixture;

impl TransactionFixture {
    pub(crate) fn limits() -> LoadLimits {
        LoadLimits {
            stack_size: 0x20_000,
            pie_hint: Some(0x90_0000),
            interpreter_hint: Some(0x60_0000),
            stack_hint: Some(0x80_0000),
            ..LoadLimits::default()
        }
    }

    pub(crate) fn request(architecture: GuestArchitecture) -> LoadRequest<'static> {
        LoadRequest {
            architecture,
            image_path: MAIN_PATH,
            executable_path: MAIN_PATH,
            arguments: &[b"/bin/program", b"--fixture"],
            environment: &[b"A=1"],
            random: [0x5a; 16],
            credentials: GuestCredentials {
                user: 1000,
                effective_user: 1000,
                group: 1000,
                effective_group: 1000,
            },
            features: GuestFeatures {
                hardware: 0x11,
                hardware_second: 0x22,
            },
        }
    }

    pub(crate) fn loader(
        architecture: GuestArchitecture,
        kind: ImageKind,
        fail_at: Option<usize>,
    ) -> Loader<FakeSource, FakeAddressSpace> {
        let source = FakeSource::new(architecture, kind);
        let address_space = FakeAddressSpace::new(fail_at);
        Loader::new(source, address_space, Self::limits())
    }

    pub(crate) fn rollback_tokens(transcript: &[Transcript]) -> Vec<u32> {
        transcript
            .iter()
            .filter_map(|event| match event {
                Transcript::Rollback(token) => Some(*token),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn reserved_tokens_in(transcript: &[Transcript]) -> Vec<u32> {
        let mut tokens: Vec<_> = transcript
            .iter()
            .filter_map(|event| match event {
                Transcript::Reserve(token, ..) => Some(*token),
                _ => None,
            })
            .collect();
        tokens.reverse();
        tokens
    }
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

pub(crate) fn fixture_with_tls(
    architecture: GuestArchitecture,
    kind: ImageKind,
    interpreter: bool,
    initialized: &[u8],
    memory_size: u64,
    alignment: u64,
) -> Vec<u8> {
    const TLS_OFFSET: usize = DATA_OFFSET + 0x40;
    let mut bytes = fixture(architecture, kind, interpreter);
    let index = if interpreter { 3 } else { 2 };
    put_u16(&mut bytes, 56, u16::try_from(index + 1).unwrap());
    bytes[TLS_OFFSET..TLS_OFFSET + initialized.len()].copy_from_slice(initialized);
    put_program_header(
        &mut bytes,
        index,
        SegmentFixture {
            kind: 7,
            flags: 4,
            offset: TLS_OFFSET as u64,
            address: LINK_BASE + 0x2040,
            file_size: initialized.len() as u64,
            memory_size,
            alignment,
        },
    );
    bytes
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
