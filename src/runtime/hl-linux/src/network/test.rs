use std::sync::Mutex;

use hl_isa::GuestArchitecture;
use hl_network::{ControlCodec, ControlMessage, ControlWord, SocketAddress, UnixAddress};

use crate::{
    GuestAccess, GuestFault, GuestMemory, GuestNetworkAddress, GuestSocketOption, MarshalError, MessageCopyoutResult,
    NetworkAbi, NetworkMarshalError,
};

const BASE: u64 = 0x1000;

struct Memory {
    bytes: Mutex<Vec<u8>>,
}

impl Memory {
    fn new() -> Self {
        Self {
            bytes: Mutex::new(vec![0; 0x4000]),
        }
    }

    fn offset(address: u64) -> Result<usize, GuestFault> {
        usize::try_from(address.checked_sub(BASE).ok_or(GuestFault {
            address,
            access: GuestAccess::Read,
        })?)
        .map_err(|_| GuestFault {
            address,
            access: GuestAccess::Read,
        })
    }

    fn put(&self, address: u64, bytes: &[u8]) {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: u64, length: usize) -> Vec<u8> {
        let offset = Self::offset(address).unwrap();
        self.bytes.lock().unwrap()[offset..offset + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let offset = Self::offset(address).map_err(|_| GuestFault { address, access })?;
        let available = self.bytes.lock().unwrap().len().saturating_sub(offset);
        if available == 0 && length != 0 {
            return Err(GuestFault { address, access });
        }
        Ok(length.min(available))
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, destination.len(), GuestAccess::Read)?;
        let offset = Self::offset(address)?;
        destination[..count].copy_from_slice(&self.bytes.lock().unwrap()[offset..offset + count]);
        Ok(count)
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        let count = self.probe(address, source.len(), GuestAccess::Write)?;
        let offset = Self::offset(address)?;
        self.bytes.lock().unwrap()[offset..offset + count].copy_from_slice(&source[..count]);
        Ok(count)
    }
}

#[test]
fn sockaddr_fixtures_isas() {
    let fixtures = [
        (
            GuestNetworkAddress::Unix(UnixAddress::Pathname(b"/run/x".to_vec())),
            vec![1, 0, b'/', b'r', b'u', b'n', b'/', b'x', 0],
        ),
        (
            GuestNetworkAddress::Unix(UnixAddress::Abstract(b"bus".to_vec())),
            vec![1, 0, 0, b'b', b'u', b's'],
        ),
        (
            GuestNetworkAddress::Inet(SocketAddress::Inet4 {
                address: [127, 0, 0, 1],
                port: 8080,
            }),
            vec![2, 0, 0x1f, 0x90, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
    ];
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let memory = Memory::new();
        let abi = NetworkAbi::new(&memory, architecture);
        for (address, bytes) in &fixtures {
            assert_eq!(NetworkAbi::<Memory>::encode_sockaddr(address).unwrap(), *bytes);
            memory.put(BASE, bytes);
            assert_eq!(abi.decode_sockaddr(BASE, bytes.len() as u32).unwrap(), *address,);
        }
    }
}

#[test]
fn sendmsg_import_control() {
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::Aarch64);
    let name = NetworkAbi::<Memory>::encode_sockaddr(&GuestNetworkAddress::Inet(SocketAddress::Inet4 {
        address: [10, 0, 0, 1],
        port: 53,
    }))
    .unwrap();
    let controls = vec![
        ControlMessage::Rights(vec![4]),
        ControlMessage::Credentials {
            process: 2,
            user: 3,
            group: 4,
        },
    ];
    let control = ControlCodec::encode(&controls, ControlWord::Eight, 128).unwrap().bytes;
    memory.put(BASE + 0x100, &name);
    memory.put(BASE + 0x200, b"abc");
    let mut iovec = Vec::new();
    iovec.extend_from_slice(&(BASE + 0x200).to_le_bytes());
    iovec.extend_from_slice(&3_u64.to_le_bytes());
    memory.put(BASE + 0x300, &iovec);
    memory.put(BASE + 0x400, &control);
    let mut header = [0_u8; 56];
    header[0..8].copy_from_slice(&(BASE + 0x100).to_le_bytes());
    header[8..12].copy_from_slice(&(name.len() as u32).to_le_bytes());
    header[16..24].copy_from_slice(&(BASE + 0x300).to_le_bytes());
    header[24..32].copy_from_slice(&1_u64.to_le_bytes());
    header[32..40].copy_from_slice(&(BASE + 0x400).to_le_bytes());
    header[40..48].copy_from_slice(&(control.len() as u64).to_le_bytes());
    memory.put(BASE, &header);

    let imported = abi.import_message(BASE, 0).unwrap();
    assert_eq!(imported.vectors.total_length, 3);
    assert_eq!(imported.controls, controls);
}

#[test]
fn recvmsg_output_name() {
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::Aarch64);
    memory.put(BASE + 0x100, &[0xff; 16]);
    memory.put(BASE + 0x200, &[0; 4]);
    let mut iovec = Vec::new();
    iovec.extend_from_slice(&(BASE + 0x200).to_le_bytes());
    iovec.extend_from_slice(&4_u64.to_le_bytes());
    memory.put(BASE + 0x300, &iovec);
    let mut header = [0_u8; 56];
    header[..8].copy_from_slice(&(BASE + 0x100).to_le_bytes());
    header[8..12].copy_from_slice(&16_u32.to_le_bytes());
    header[16..24].copy_from_slice(&(BASE + 0x300).to_le_bytes());
    header[24..32].copy_from_slice(&1_u64.to_le_bytes());
    memory.put(BASE, &header);

    let imported = abi.import_receive_message(BASE, 0).unwrap();
    assert_eq!(imported.header.name, BASE + 0x100);
    assert_eq!(imported.address, None);
}

#[test]
fn recvmsg_plan_copyout() {
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::X86_64);
    let mut iovecs = Vec::new();
    iovecs.extend_from_slice(&(BASE + 0x200).to_le_bytes());
    iovecs.extend_from_slice(&2_u64.to_le_bytes());
    iovecs.extend_from_slice(&u64::MAX.to_le_bytes());
    iovecs.extend_from_slice(&2_u64.to_le_bytes());
    memory.put(BASE + 0x100, &iovecs);
    let mut header = [0_u8; 56];
    header[16..24].copy_from_slice(&(BASE + 0x100).to_le_bytes());
    header[24..32].copy_from_slice(&2_u64.to_le_bytes());
    memory.put(BASE, &header);
    let imported = abi.import_receive_message(BASE, 0).unwrap_err();
    assert!(matches!(imported, NetworkMarshalError::Marshal(_),));
    assert_eq!(memory.get(BASE + 0x200, 2), vec![0, 0]);
}

#[test]
fn recvmsg_copyout_prepare() {
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::X86_64);
    let mut iovec = Vec::new();
    iovec.extend_from_slice(&(BASE + 0x200).to_le_bytes());
    iovec.extend_from_slice(&4_u64.to_le_bytes());
    memory.put(BASE + 0x100, &iovec);
    let mut header = [0_u8; 56];
    header[16..24].copy_from_slice(&(BASE + 0x100).to_le_bytes());
    header[24..32].copy_from_slice(&1_u64.to_le_bytes());
    memory.put(BASE, &header);
    let imported = abi.import_receive_message(BASE, 0).unwrap();
    let plan = abi
        .prepare_receive(
            &imported,
            &MessageCopyoutResult {
                address: None,
                data: b"rust".to_vec(),
                controls: Vec::new(),
                flags: 0x20,
            },
        )
        .unwrap();
    assert_eq!(memory.get(BASE + 0x200, 4), vec![0; 4]);
    plan.commit(&crate::GuestMarshaller::new(&memory, GuestArchitecture::X86_64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x200, 4), b"rust");
    assert_eq!(memory.get(BASE + 48, 4), 0x20_u32.to_le_bytes());
}

#[test]
fn sockaddr_copyout_length() {
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::Aarch64);
    let address = GuestNetworkAddress::Inet(SocketAddress::Inet4 {
        address: [1, 2, 3, 4],
        port: 80,
    });
    assert!(matches!(
        abi.prepare_sockaddr_copyout(BASE + 0x100, u64::MAX, &address),
        Err(NetworkMarshalError::Marshal(_)),
    ));
    assert_eq!(memory.get(BASE + 0x100, 4), vec![0; 4]);

    memory.put(BASE + 0x80, &4_u32.to_le_bytes());
    let plan = abi
        .prepare_sockaddr_copyout(BASE + 0x100, BASE + 0x80, &address)
        .unwrap();
    plan.commit(&crate::GuestMarshaller::new(&memory, GuestArchitecture::Aarch64))
        .unwrap();
    assert_eq!(memory.get(BASE + 0x100, 4), vec![2, 0, 0, 80]);
    assert_eq!(memory.get(BASE + 0x80, 4), 16_u32.to_le_bytes());

    memory.put(BASE + 0x80, &4096_u32.to_le_bytes());
    assert!(
        abi.prepare_sockaddr_copyout(BASE + 0x100, BASE + 0x80, &address,)
            .is_ok()
    );
    memory.put(BASE + 0x80, &4097_u32.to_le_bytes());
    assert_eq!(
        abi.prepare_sockaddr_copyout(BASE + 0x100, BASE + 0x80, &address),
        Err(NetworkMarshalError::Marshal(MarshalError::Invalid)),
    );

    memory.put(BASE + 0x80, &16_u32.to_le_bytes());
    assert!(matches!(
        abi.prepare_sockaddr_copyout(BASE + 0x3ff8, BASE + 0x80, &address),
        Err(NetworkMarshalError::Marshal(MarshalError::Fault(GuestFault {
            address: 0x5000,
            access: GuestAccess::Write,
        }))),
    ));
}

#[test]
fn socket_option_layouts() {
    let forms = [
        GuestSocketOption::Scalar(7),
        GuestSocketOption::Timeval {
            seconds: 2,
            microseconds: 3,
        },
        GuestSocketOption::Linger { enabled: 1, seconds: 9 },
        GuestSocketOption::Credentials {
            process: 4,
            user: 5,
            group: 6,
        },
    ];
    let memory = Memory::new();
    let abi = NetworkAbi::new(&memory, GuestArchitecture::X86_64);
    for form in forms {
        let bytes = NetworkAbi::<Memory>::encode_socket_option(form.clone());
        memory.put(BASE, &bytes);
        assert_eq!(
            abi.decode_socket_option(BASE, bytes.len() as u32, form.clone())
                .unwrap(),
            form,
        );
    }
}
