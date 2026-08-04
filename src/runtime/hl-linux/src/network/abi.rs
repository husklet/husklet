use hl_isa::GuestArchitecture;
use hl_network::{
    ControlCodec, ControlEncoding, ControlError, ControlMessage, ControlWord, SocketAddress, UnixAddress,
};

use crate::{GuestAccess, GuestFault, GuestIovec, GuestMarshaller, GuestMemory, IOV_MAXIMUM, IovecPlan, MarshalError};

const SOCKADDR_MAXIMUM: usize = 128;
const SOCKADDR_COPYOUT_CAPACITY_MAXIMUM: u32 = 4096;
const MESSAGE_HEADER_SIZE: usize = 56;
const MESSAGE_FLAG_MASK: u32 = 0x4000_7fff;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestNetworkAddress {
    Unspecified,
    Unix(UnixAddress),
    Inet(SocketAddress),
    Netlink { port: u32, groups: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Marshal(MarshalError),
    Control(ControlError),
    InvalidFamily,
    InvalidLength,
    InvalidFlags,
    TooManyVectors,
}

impl From<MarshalError> for Error {
    fn from(error: MarshalError) -> Self {
        Self::Marshal(error)
    }
}

impl From<ControlError> for Error {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestMessageHeader {
    pub name: u64,
    pub name_length: u32,
    pub iovecs: u64,
    pub iovec_count: usize,
    pub control: u64,
    pub control_length: usize,
    pub flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageImport {
    pub header_address: u64,
    pub header: GuestMessageHeader,
    pub address: Option<GuestNetworkAddress>,
    pub vectors: IovecPlan,
    pub controls: Vec<ControlMessage>,
    pub call_flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageCopyoutResult {
    pub address: Option<GuestNetworkAddress>,
    pub data: Vec<u8>,
    pub controls: Vec<ControlMessage>,
    pub flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CopyoutWrite {
    address: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageCopyout {
    writes: Vec<CopyoutWrite>,
}

impl MessageCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), Error> {
        for write in self.writes {
            let progress = marshaller.copy_to(write.address, &write.bytes);
            if let Some(fault) = progress.fault {
                return Err(Error::Marshal(MarshalError::Fault(fault)));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestSocketOption {
    Scalar(i32),
    Timeval { seconds: i64, microseconds: i64 },
    Linger { enabled: i32, seconds: i32 },
    Credentials { process: u32, user: u32, group: u32 },
    Bytes(Vec<u8>),
    Filter(Vec<crate::BpfInstruction>),
}

pub struct Abi<'a, M: GuestMemory> {
    marshaller: GuestMarshaller<'a, M>,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
        }
    }

    pub fn decode_sockaddr(&self, source: u64, length: u32) -> Result<GuestNetworkAddress, Error> {
        if length as usize > SOCKADDR_MAXIMUM || length < 2 {
            return Err(Error::InvalidLength);
        }
        let mut bytes = vec![0; length as usize];
        let progress = self.marshaller.copy_from(source, &mut bytes);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault).into());
        }
        match u16::from_le_bytes(bytes[0..2].try_into().unwrap()) {
            0 => Ok(GuestNetworkAddress::Unspecified),
            1 => Self::decode_unix(&bytes),
            2 if bytes.len() >= 16 => Ok(GuestNetworkAddress::Inet(SocketAddress::Inet4 {
                port: u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
                address: bytes[4..8].try_into().unwrap(),
            })),
            10 if bytes.len() >= 28 => Ok(GuestNetworkAddress::Inet(SocketAddress::Inet6 {
                port: u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
                address: bytes[8..24].try_into().unwrap(),
                scope: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            })),
            16 if bytes.len() >= 12 => Ok(GuestNetworkAddress::Netlink {
                port: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                groups: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            }),
            2 | 10 => Err(Error::InvalidLength),
            _ => Err(Error::InvalidFamily),
        }
    }

    fn decode_unix(bytes: &[u8]) -> Result<GuestNetworkAddress, Error> {
        let path = &bytes[2..];
        if path.is_empty() {
            return Ok(GuestNetworkAddress::Unix(UnixAddress::Unnamed));
        }
        if path[0] == 0 {
            return Ok(GuestNetworkAddress::Unix(UnixAddress::Abstract(path[1..].to_vec())));
        }
        let end = path.iter().position(|byte| *byte == 0).unwrap_or(path.len());
        Ok(GuestNetworkAddress::Unix(UnixAddress::Pathname(path[..end].to_vec())))
    }

    pub fn encode_sockaddr(address: &GuestNetworkAddress) -> Result<Vec<u8>, Error> {
        match address {
            GuestNetworkAddress::Unspecified => Ok(vec![0; 16]),
            GuestNetworkAddress::Unix(address) => Self::encode_unix(address),
            GuestNetworkAddress::Inet(SocketAddress::Inet4 { address, port }) => {
                let mut bytes = vec![0; 16];
                bytes[..2].copy_from_slice(&2_u16.to_le_bytes());
                bytes[2..4].copy_from_slice(&port.to_be_bytes());
                bytes[4..8].copy_from_slice(address);
                Ok(bytes)
            }
            GuestNetworkAddress::Inet(SocketAddress::Inet6 { address, port, scope }) => {
                let mut bytes = vec![0; 28];
                bytes[..2].copy_from_slice(&10_u16.to_le_bytes());
                bytes[2..4].copy_from_slice(&port.to_be_bytes());
                bytes[8..24].copy_from_slice(address);
                bytes[24..28].copy_from_slice(&scope.to_le_bytes());
                Ok(bytes)
            }
            GuestNetworkAddress::Inet(SocketAddress::Unix(_)) => Err(Error::InvalidFamily),
            GuestNetworkAddress::Netlink { port, groups } => {
                let mut bytes = vec![0; 12];
                bytes[..2].copy_from_slice(&16_u16.to_le_bytes());
                bytes[4..8].copy_from_slice(&port.to_le_bytes());
                bytes[8..12].copy_from_slice(&groups.to_le_bytes());
                Ok(bytes)
            }
        }
    }

    fn encode_unix(address: &UnixAddress) -> Result<Vec<u8>, Error> {
        let mut bytes = 1_u16.to_le_bytes().to_vec();
        match address {
            UnixAddress::Unnamed => {}
            UnixAddress::Pathname(path) => {
                if path.is_empty() || path.len() > 107 {
                    return Err(Error::InvalidLength);
                }
                bytes.extend_from_slice(path);
                bytes.push(0);
            }
            UnixAddress::Abstract(name) => {
                if name.is_empty() || name.len() > 107 {
                    return Err(Error::InvalidLength);
                }
                bytes.push(0);
                bytes.extend_from_slice(name);
            }
        }
        Ok(bytes)
    }

    pub fn prepare_sockaddr_copyout(
        &self,
        address_pointer: u64,
        length_pointer: u64,
        address: &GuestNetworkAddress,
    ) -> Result<MessageCopyout, Error> {
        let capacity = self
            .marshaller
            .socklen(length_pointer, SOCKADDR_COPYOUT_CAPACITY_MAXIMUM)? as usize;
        let encoded = Self::encode_sockaddr(address)?;
        let count = capacity.min(encoded.len());
        let writes = vec![
            CopyoutWrite {
                address: address_pointer,
                bytes: encoded[..count].to_vec(),
            },
            CopyoutWrite {
                address: length_pointer,
                bytes: (encoded.len() as u32).to_le_bytes().to_vec(),
            },
        ];
        for write in &writes {
            let available = self
                .marshaller
                .probe(write.address, write.bytes.len(), GuestAccess::Write)?;
            if available != write.bytes.len() {
                return Err(Error::Marshal(MarshalError::Fault(GuestFault {
                    address: write.address.saturating_add(available as u64),
                    access: GuestAccess::Write,
                })));
            }
        }
        Ok(MessageCopyout { writes })
    }

    pub fn import_message(&self, header_address: u64, call_flags: u32) -> Result<MessageImport, Error> {
        self.import_message_for(header_address, call_flags, GuestAccess::Read)
    }

    pub fn import_receive_message(&self, header_address: u64, call_flags: u32) -> Result<MessageImport, Error> {
        self.import_message_for(header_address, call_flags, GuestAccess::Write)
    }

    fn import_message_for(
        &self,
        header_address: u64,
        call_flags: u32,
        vector_access: GuestAccess,
    ) -> Result<MessageImport, Error> {
        Self::validate_flags(call_flags)?;
        let bytes = self
            .marshaller
            .copy_struct_from::<MESSAGE_HEADER_SIZE>(header_address)?;
        let header = Self::decode_header(&bytes)?;
        if header.iovec_count > IOV_MAXIMUM {
            return Err(Error::TooManyVectors);
        }
        let vectors = self.marshaller.iovecs(header.iovecs, header.iovec_count)?;
        for vector in &vectors.vectors {
            self.marshaller.probe(
                vector.base,
                usize::try_from(vector.length).map_err(|_| Error::InvalidLength)?,
                vector_access,
            )?;
        }
        let address = if vector_access == GuestAccess::Write || header.name == 0 || header.name_length == 0 {
            None
        } else {
            Some(self.decode_sockaddr(header.name, header.name_length)?)
        };
        let controls = if vector_access == GuestAccess::Write || header.control == 0 || header.control_length == 0 {
            Vec::new()
        } else {
            let mut control = vec![0; header.control_length];
            let progress = self.marshaller.copy_from(header.control, &mut control);
            if let Some(fault) = progress.fault {
                return Err(MarshalError::Fault(fault).into());
            }
            ControlCodec::decode(&control, ControlWord::Eight)?
        };
        Ok(MessageImport {
            header_address,
            header,
            address,
            vectors,
            controls,
            call_flags,
        })
    }

    fn decode_header(bytes: &[u8; MESSAGE_HEADER_SIZE]) -> Result<GuestMessageHeader, Error> {
        let iovec_count = usize::try_from(Self::u64(bytes, 24)).map_err(|_| Error::InvalidLength)?;
        let control_length = usize::try_from(Self::u64(bytes, 40)).map_err(|_| Error::InvalidLength)?;
        let flags = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
        Ok(GuestMessageHeader {
            name: Self::u64(bytes, 0),
            name_length: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            iovecs: Self::u64(bytes, 16),
            iovec_count,
            control: Self::u64(bytes, 32),
            control_length,
            flags,
        })
    }

    pub fn prepare_receive(
        &self,
        imported: &MessageImport,
        result: &MessageCopyoutResult,
    ) -> Result<MessageCopyout, Error> {
        Self::validate_flags(result.flags)?;
        let mut writes = Vec::new();
        Self::scatter_data(&mut writes, &imported.vectors.vectors, &result.data);
        let address = result.address.as_ref().map(Self::encode_sockaddr).transpose()?;
        let actual_name_length = address.as_ref().map_or(0, Vec::len);
        if let Some(address) = address {
            let count = address.len().min(imported.header.name_length as usize);
            if imported.header.name != 0 && count > 0 {
                writes.push(CopyoutWrite {
                    address: imported.header.name,
                    bytes: address[..count].to_vec(),
                });
            }
        }
        let ControlEncoding {
            bytes: control,
            truncated,
        } = ControlCodec::encode(&result.controls, ControlWord::Eight, imported.header.control_length)?;
        if imported.header.control != 0 && !control.is_empty() {
            writes.push(CopyoutWrite {
                address: imported.header.control,
                bytes: control.clone(),
            });
        }
        let mut header = self
            .marshaller
            .copy_struct_from::<MESSAGE_HEADER_SIZE>(imported.header_address)?;
        header[8..12].copy_from_slice(&(actual_name_length as u32).to_le_bytes());
        header[40..48].copy_from_slice(&(control.len() as u64).to_le_bytes());
        let output_flags = result.flags | if truncated { 0x8 } else { 0 };
        header[48..52].copy_from_slice(&output_flags.to_le_bytes());
        writes.push(CopyoutWrite {
            address: imported.header_address,
            bytes: header.to_vec(),
        });
        for write in &writes {
            let available = self
                .marshaller
                .probe(write.address, write.bytes.len(), GuestAccess::Write)?;
            if available != write.bytes.len() {
                return Err(Error::InvalidLength);
            }
        }
        Ok(MessageCopyout { writes })
    }

    fn scatter_data(writes: &mut Vec<CopyoutWrite>, vectors: &[GuestIovec], data: &[u8]) {
        let mut offset = 0;
        for vector in vectors {
            let count = (vector.length as usize).min(data.len() - offset);
            if count == 0 {
                break;
            }
            writes.push(CopyoutWrite {
                address: vector.base,
                bytes: data[offset..offset + count].to_vec(),
            });
            offset += count;
        }
    }

    pub fn decode_socket_option(
        &self,
        source: u64,
        length: u32,
        form: GuestSocketOption,
    ) -> Result<GuestSocketOption, Error> {
        let required = match form {
            GuestSocketOption::Scalar(_) => 4,
            GuestSocketOption::Timeval { .. } => 16,
            GuestSocketOption::Linger { .. } => 8,
            GuestSocketOption::Credentials { .. } => 12,
            GuestSocketOption::Bytes(ref bytes) => bytes.len(),
            GuestSocketOption::Filter(_) => return Err(Error::InvalidLength),
        };
        if length as usize != required {
            return Err(Error::InvalidLength);
        }
        let mut bytes = vec![0; required];
        let progress = self.marshaller.copy_from(source, &mut bytes);
        if let Some(fault) = progress.fault {
            return Err(MarshalError::Fault(fault).into());
        }
        Ok(match form {
            GuestSocketOption::Scalar(_) => {
                GuestSocketOption::Scalar(i32::from_le_bytes(bytes[..4].try_into().unwrap()))
            }
            GuestSocketOption::Timeval { .. } => GuestSocketOption::Timeval {
                seconds: i64::from_le_bytes(bytes[..8].try_into().unwrap()),
                microseconds: i64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            },
            GuestSocketOption::Linger { .. } => GuestSocketOption::Linger {
                enabled: i32::from_le_bytes(bytes[..4].try_into().unwrap()),
                seconds: i32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            },
            GuestSocketOption::Credentials { .. } => GuestSocketOption::Credentials {
                process: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                user: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                group: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            },
            GuestSocketOption::Bytes(_) => GuestSocketOption::Bytes(bytes),
            GuestSocketOption::Filter(_) => unreachable!("filter programs require nested-pointer decoding"),
        })
    }

    #[must_use]
    pub fn encode_socket_option(option: GuestSocketOption) -> Vec<u8> {
        match option {
            GuestSocketOption::Scalar(value) => value.to_le_bytes().to_vec(),
            GuestSocketOption::Timeval { seconds, microseconds } => {
                [seconds.to_le_bytes(), microseconds.to_le_bytes()].concat()
            }
            GuestSocketOption::Linger { enabled, seconds } => [enabled.to_le_bytes(), seconds.to_le_bytes()].concat(),
            GuestSocketOption::Credentials { process, user, group } => {
                [process.to_le_bytes(), user.to_le_bytes(), group.to_le_bytes()].concat()
            }
            GuestSocketOption::Bytes(bytes) => bytes,
            GuestSocketOption::Filter(_) => Vec::new(),
        }
    }

    fn validate_flags(flags: u32) -> Result<(), Error> {
        if flags & !MESSAGE_FLAG_MASK != 0 {
            Err(Error::InvalidFlags)
        } else {
            Ok(())
        }
    }

    fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }
}
