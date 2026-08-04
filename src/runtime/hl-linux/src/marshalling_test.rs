use std::sync::Mutex;

use hl_isa::GuestArchitecture;

use crate::{
    Errno, GuestAccess, GuestFault, GuestMarshaller, GuestMemory, IOV_MAXIMUM, MAX_RW_COUNT, MarshalError,
    USER_ADDRESS_LIMIT,
};

const BASE: u64 = 0x1000;
const ALIAS: u64 = 0x9000;

struct FaultMemory {
    bytes: Mutex<Vec<u8>>,
    denied_read_page: Option<usize>,
    denied_write_page: Option<usize>,
}

impl FaultMemory {
    fn new(length: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; length]),
            denied_read_page: None,
            denied_write_page: None,
        }
    }

    fn with_denied(length: usize, read_page: Option<usize>, write_page: Option<usize>) -> Self {
        Self {
            bytes: Mutex::new(vec![0; length]),
            denied_read_page: read_page,
            denied_write_page: write_page,
        }
    }

    fn offset(address: u64) -> Option<usize> {
        let canonical = if address >= ALIAS {
            address.checked_sub(ALIAS)?.checked_add(BASE)?
        } else {
            address
        };
        usize::try_from(canonical.checked_sub(BASE)?).ok()
    }

    fn span(&self, address: u64, length: usize, access: GuestAccess) -> Result<(usize, usize), GuestFault> {
        let offset = Self::offset(address).ok_or(GuestFault { address, access })?;
        let page = offset / 4096;
        let denied = match access {
            GuestAccess::Read => self.denied_read_page,
            GuestAccess::Write => self.denied_write_page,
        };
        let bytes = self.bytes.lock().unwrap();
        if denied == Some(page) || offset >= bytes.len() {
            return Err(GuestFault { address, access });
        }
        let page_left = 4096 - offset % 4096;
        Ok((offset, length.min(page_left).min(bytes.len() - offset)))
    }

    fn put(&self, address: u64, source: &[u8]) {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + source.len()].copy_from_slice(source);
    }

    fn get(&self, address: u64, length: usize) -> Vec<u8> {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + length].to_vec()
    }

    fn put_vectors(&self, vectors: &[(u64, u64)]) {
        for (index, (base, length)) in vectors.iter().enumerate() {
            let address = BASE + (index * 16) as u64;
            self.put(address, &base.to_le_bytes());
            self.put(address + 8, &length.to_le_bytes());
        }
    }
}

impl GuestMemory for FaultMemory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        self.span(address, length, access).map(|(_, span)| span)
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        let (offset, span) = self.span(address, destination.len(), GuestAccess::Read)?;
        destination[..span].copy_from_slice(&self.bytes.lock().unwrap()[offset..offset + span]);
        Ok(span)
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        let (offset, span) = self.span(address, source.len(), GuestAccess::Write)?;
        self.bytes.lock().unwrap()[offset..offset + span].copy_from_slice(&source[..span]);
        Ok(span)
    }
}

#[test]
fn page_boundary_coordinate() {
    let memory = FaultMemory::with_denied(8192, Some(1), Some(1));
    memory.put(BASE + 4090, &[1, 2, 3, 4, 5, 6]);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    let mut destination = [0xaa; 12];
    let progress = marshaller.copy_from(BASE + 4090, &mut destination);

    assert_eq!(progress.copied, 6);
    assert_eq!(
        progress.fault,
        Some(GuestFault {
            address: BASE + 4096,
            access: GuestAccess::Read,
        })
    );
    assert_eq!(&destination[..6], &[1, 2, 3, 4, 5, 6]);
    assert_eq!(&destination[6..], &[0xaa; 6]);
}

#[test]
fn writes_stop_page() {
    let memory = FaultMemory::with_denied(8192, None, Some(1));
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::X86_64);
    let progress = marshaller.copy_to(BASE + 4092, &[7; 12]);

    assert_eq!(progress.copied, 4);
    assert_eq!(memory.get(BASE + 4092, 4), vec![7; 4]);
    assert_eq!(memory.get(BASE + 4096, 8), vec![0; 8]);
}

#[test]
fn aliases_share_contract() {
    let memory = FaultMemory::new(4096);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(marshaller.copy_to(ALIAS + 8, b"alias").copied, 5);
    let mut result = [0; 5];
    assert_eq!(marshaller.copy_from(BASE + 8, &mut result).copied, 5);
    assert_eq!(&result, b"alias");
}

#[test]
fn c_strings_limit() {
    let memory = FaultMemory::new(4096);
    memory.put(BASE, b"hello\0unterminated");
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(marshaller.c_string(BASE, 6), Ok(b"hello".to_vec()));
    assert_eq!(marshaller.c_string(BASE + 6, 5), Err(MarshalError::TooBig));
    assert!(matches!(marshaller.c_string(0, 5), Err(MarshalError::Fault(_))));
}

#[test]
fn pointer_vectors_e2big() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = FaultMemory::new(4096);
        let words = [0x1234_u64, 0x5678, 0];
        for (index, word) in words.iter().enumerate() {
            memory.put(BASE + (index * 8) as u64, &word.to_le_bytes());
        }
        let marshaller = GuestMarshaller::new(&memory, architecture);
        assert_eq!(marshaller.pointer_vector(BASE, 3), Ok(vec![0x1234, 0x5678]));
        assert_eq!(marshaller.pointer_vector(BASE, 2), Err(MarshalError::TooBig));
    }
}

#[test]
fn iovec_validation_overflow() {
    let memory = FaultMemory::new(4096);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(marshaller.iovecs(u64::MAX, IOV_MAXIMUM + 1), Err(MarshalError::Invalid));
    memory.put(BASE, &1_u64.to_le_bytes());
    memory.put(BASE + 8, &u64::MAX.to_le_bytes());
    memory.put(BASE + 16, &2_u64.to_le_bytes());
    memory.put(BASE + 24, &1_u64.to_le_bytes());
    assert_eq!(marshaller.iovecs(BASE, 2), Err(MarshalError::Overflow));
}

#[test]
fn vectors_truncate_limit() {
    let memory = FaultMemory::new(4096);
    memory.put_vectors(&[(BASE, MAX_RW_COUNT - 2), (BASE, 10), (BASE, 5)]);
    let plan = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64)
        .io_vectors(BASE, 3, GuestAccess::Read)
        .unwrap();
    assert_eq!(plan.total_length, MAX_RW_COUNT);
    assert_eq!(
        plan.vectors.iter().map(|vector| vector.length).collect::<Vec<_>>(),
        [MAX_RW_COUNT - 2, 2, 0],
    );
}

#[test]
fn vectors_reject_signed() {
    let memory = FaultMemory::new(4096);
    memory.put_vectors(&[(BASE, i64::MAX as u64 + 1)]);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(
        marshaller.io_vectors(BASE, 1, GuestAccess::Read),
        Err(MarshalError::Invalid),
    );
}

#[test]
fn vectors_validate_range() {
    for (base, length) in [(u64::MAX - 7, 16), (USER_ADDRESS_LIMIT - 4, 8)] {
        let memory = FaultMemory::new(4096);
        memory.put_vectors(&[(base, length)]);
        let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
        assert_eq!(
            marshaller.io_vectors(BASE, 1, GuestAccess::Write),
            Err(MarshalError::Fault(GuestFault {
                address: base,
                access: GuestAccess::Write,
            })),
        );
    }
}

#[test]
fn vector_records_validate_range_before_transfer_bound() {
    let memory = FaultMemory::new(4096);
    memory.put_vectors(&[(BASE, i64::MAX as u64), (BASE, i64::MAX as u64)]);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    assert!(matches!(
        marshaller.io_vector_records(BASE, 2, GuestAccess::Read),
        Err(MarshalError::Fault(_))
    ));
}

#[test]
fn vectors_allow_empty() {
    let memory = FaultMemory::new(4096);
    memory.put_vectors(&[(u64::MAX, 0)]);
    let plan = GuestMarshaller::new(&memory, GuestArchitecture::X86_64)
        .io_vectors(BASE, 1, GuestAccess::Read)
        .unwrap();
    assert_eq!(plan.total_length, 0);
    assert_eq!(plan.vectors[0].base, u64::MAX);
}

#[test]
fn vectors_bound_first() {
    let memory = FaultMemory::new(0);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    assert_eq!(
        marshaller.io_vectors(u64::MAX, IOV_MAXIMUM + 1, GuestAccess::Read),
        Err(MarshalError::Invalid),
    );
}

#[test]
fn socklen_validates_mutating() {
    let memory = FaultMemory::with_denied(8192, None, Some(1));
    memory.put(BASE, &129_u32.to_le_bytes());
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::X86_64);
    assert_eq!(marshaller.socklen(BASE, 128), Err(MarshalError::Invalid));
    assert!(matches!(
        marshaller.write_socklen(BASE + 4096, 12),
        Err(MarshalError::Fault(_))
    ));
    assert_eq!(memory.get(BASE + 4096, 4), vec![0; 4]);
}

#[test]
fn fixed_struct_writing() {
    let memory = FaultMemory::with_denied(8192, None, Some(1));
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    let structure = [0x44; 16];
    assert!(matches!(
        marshaller.copy_struct_to(BASE + 4088, &structure),
        Err(MarshalError::Fault(_))
    ));
    assert_eq!(memory.get(BASE + 4088, 8), vec![0; 8]);
    memory.put(BASE + 32, &structure);
    assert_eq!(marshaller.copy_struct_from::<16>(BASE + 32), Ok(structure));
}

#[test]
fn marshalling_failures_values() {
    let fault = GuestFault {
        address: 1,
        access: GuestAccess::Read,
    };
    assert_eq!(MarshalError::Fault(fault).errno(), Errno::EFAULT);
    assert_eq!(MarshalError::Invalid.errno(), Errno::EINVAL);
    assert_eq!(MarshalError::TooBig.errno(), Errno::E2BIG);
    assert_eq!(MarshalError::Overflow.errno(), Errno::EOVERFLOW);
}

#[test]
fn user_address_overflow_is_a_fault() {
    let memory = FaultMemory::with_denied(8192, None, None);
    let marshaller = GuestMarshaller::new(&memory, GuestArchitecture::Aarch64);
    assert!(matches!(
        marshaller.probe(u64::MAX, 16, GuestAccess::Write),
        Err(MarshalError::Fault(GuestFault {
            address: u64::MAX,
            access: GuestAccess::Write,
        }))
    ));
    assert!(matches!(marshaller.c_string(u64::MAX, 2), Err(MarshalError::Fault(_))));
}
