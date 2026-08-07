//! Checkpoint image and broker/channel wire contracts.

#![forbid(unsafe_code)]

use std::fmt;
use std::io::{Read, Write};

mod codec;
mod io;

use codec::{DecodeError, Decoder, EncodeError, Encoder};

mod image;
mod image_error;
mod port;

pub use image::{
    CheckpointImage, CheckpointReader, CheckpointWriter, ImageLimits, PreparedImage, Section, SectionKind,
};
pub use image_error::{ChecksumRegion, ImageError};
pub use port::{CheckpointSink, CheckpointSource, Fault, MemorySink, MemorySource, PortError};

pub const MAGIC_HELLO: u32 = 0x484b_4348;
pub const MAGIC_REQUEST: u32 = 0x484b_4351;
pub const MAGIC_REPLY: u32 = 0x484b_4353;
pub const STREAM_ABI: u32 = 1;
pub const NAME_MAX: usize = 512;
pub const PAYLOAD_MAX: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Operation {
    ObjectBegin = 1,
    ObjectWrite = 2,
    ObjectWriteAt = 3,
    ObjectTell = 4,
    ObjectFinish = 5,
    ObjectAbort = 6,
    GroupBegin = 7,
    GroupCommit = 8,
    GroupAbort = 9,
    Claim = 10,
    Unclaim = 11,
    Commit = 12,
    GroupPresent = 13,
    GroupCount = 14,
    Digest = 15,
    SourceList = 16,
    SourceSize = 17,
    SourceRead = 18,
}

impl Operation {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::ObjectBegin,
            2 => Self::ObjectWrite,
            3 => Self::ObjectWriteAt,
            4 => Self::ObjectTell,
            5 => Self::ObjectFinish,
            6 => Self::ObjectAbort,
            7 => Self::GroupBegin,
            8 => Self::GroupCommit,
            9 => Self::GroupAbort,
            10 => Self::Claim,
            11 => Self::Unclaim,
            12 => Self::Commit,
            13 => Self::GroupPresent,
            14 => Self::GroupCount,
            15 => Self::Digest,
            16 => Self::SourceList,
            17 => Self::SourceSize,
            18 => Self::SourceRead,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hello {
    pub host_pid: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: Operation,
    pub flags: u32,
    pub stream: u64,
    pub offset: u64,
    pub length: u64,
    pub name_size: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ReplyStatus {
    Error = -1,
    Ok = 0,
    Already = 1,
}

impl ReplyStatus {
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            -1 => Some(Self::Error),
            0 => Some(Self::Ok),
            1 => Some(Self::Already),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reply {
    pub status: ReplyStatus,
    pub value: u64,
    pub length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Digest {
    pub hash: u64,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum WireError {
    Encode(EncodeError),
    Decode(DecodeError),
    Magic { expected: u32, actual: u32 },
    Abi { expected: u32, actual: u32 },
    Reserved(u32),
    Operation(u32),
    Status(i32),
    NameTooLong(u32),
    PayloadTooLarge(u64),
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => error.fmt(formatter),
            Self::Decode(error) => error.fmt(formatter),
            Self::Magic { expected, actual } => {
                write!(formatter, "wire magic {actual:#x}, expected {expected:#x}")
            }
            Self::Abi { expected, actual } => {
                write!(formatter, "wire ABI {actual}, expected {expected}")
            }
            Self::Reserved(value) => write!(formatter, "reserved field is {value}"),
            Self::Operation(value) => write!(formatter, "unknown operation {value}"),
            Self::Status(value) => write!(formatter, "unknown reply status {value}"),
            Self::NameTooLong(value) => write!(formatter, "name size {value} is too large"),
            Self::PayloadTooLarge(value) => {
                write!(formatter, "payload size {value} is too large")
            }
        }
    }
}

impl std::error::Error for WireError {}

impl From<EncodeError> for WireError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl From<DecodeError> for WireError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl Hello {
    pub fn encode<W: Write>(&self, output: &mut Encoder<W>) -> Result<(), WireError> {
        output.write_u32(MAGIC_HELLO)?;
        output.write_u32(STREAM_ABI)?;
        output.write_u64(self.host_pid)?;
        Ok(())
    }

    pub fn decode<R: Read>(input: &mut Decoder<R>) -> Result<Self, WireError> {
        Header::decode(input, MAGIC_HELLO)?;
        Ok(Self {
            host_pid: input.read_u64()?,
        })
    }
}

impl Request {
    pub fn encode<W: Write>(&self, output: &mut Encoder<W>) -> Result<(), WireError> {
        self.validate_lengths()?;
        output.write_u32(MAGIC_REQUEST)?;
        output.write_u32(STREAM_ABI)?;
        output.write_u32(self.operation as u32)?;
        output.write_u32(self.flags)?;
        output.write_u64(self.stream)?;
        output.write_u64(self.offset)?;
        output.write_u64(self.length)?;
        output.write_u32(self.name_size)?;
        output.write_u32(0)?;
        Ok(())
    }

    pub fn decode<R: Read>(input: &mut Decoder<R>) -> Result<Self, WireError> {
        Header::decode(input, MAGIC_REQUEST)?;
        let raw_operation = input.read_u32()?;
        let request = Self {
            operation: Operation::from_raw(raw_operation).ok_or(WireError::Operation(raw_operation))?,
            flags: input.read_u32()?,
            stream: input.read_u64()?,
            offset: input.read_u64()?,
            length: input.read_u64()?,
            name_size: input.read_u32()?,
        };
        let reserved = input.read_u32()?;
        if reserved != 0 {
            return Err(WireError::Reserved(reserved));
        }
        request.validate_lengths()?;
        Ok(request)
    }

    fn validate_lengths(&self) -> Result<(), WireError> {
        if usize::try_from(self.name_size).unwrap_or(usize::MAX) > NAME_MAX {
            return Err(WireError::NameTooLong(self.name_size));
        }
        if self.length > PAYLOAD_MAX as u64 {
            return Err(WireError::PayloadTooLarge(self.length));
        }
        Ok(())
    }
}

impl Reply {
    pub fn encode<W: Write>(&self, output: &mut Encoder<W>) -> Result<(), WireError> {
        if self.length > PAYLOAD_MAX as u64 {
            return Err(WireError::PayloadTooLarge(self.length));
        }
        output.write_u32(MAGIC_REPLY)?;
        output.write_u32(STREAM_ABI)?;
        output.write_i32(self.status as i32)?;
        output.write_u32(0)?;
        output.write_u64(self.value)?;
        output.write_u64(self.length)?;
        Ok(())
    }

    pub fn decode<R: Read>(input: &mut Decoder<R>) -> Result<Self, WireError> {
        Header::decode(input, MAGIC_REPLY)?;
        let raw_status = input.read_i32()?;
        let status = ReplyStatus::from_raw(raw_status).ok_or(WireError::Status(raw_status))?;
        let reserved = input.read_u32()?;
        if reserved != 0 {
            return Err(WireError::Reserved(reserved));
        }
        let reply = Self {
            status,
            value: input.read_u64()?,
            length: input.read_u64()?,
        };
        if reply.length > PAYLOAD_MAX as u64 {
            return Err(WireError::PayloadTooLarge(reply.length));
        }
        Ok(reply)
    }
}

impl Digest {
    pub fn encode<W: Write>(&self, output: &mut Encoder<W>) -> Result<(), WireError> {
        output.write_u64(self.hash)?;
        output.write_u64(self.files)?;
        output.write_u64(self.bytes)?;
        Ok(())
    }

    pub fn decode<R: Read>(input: &mut Decoder<R>) -> Result<Self, WireError> {
        Ok(Self {
            hash: input.read_u64()?,
            files: input.read_u64()?,
            bytes: input.read_u64()?,
        })
    }
}

struct Header;

impl Header {
    fn decode<R: Read>(input: &mut Decoder<R>, expected_magic: u32) -> Result<(), WireError> {
        let magic = input.read_u32()?;
        if magic != expected_magic {
            return Err(WireError::Magic {
                expected: expected_magic,
                actual: magic,
            });
        }
        let abi = input.read_u32()?;
        if abi != STREAM_ABI {
            return Err(WireError::Abi {
                expected: STREAM_ABI,
                actual: abi,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::codec::Limits;

    use super::*;

    fn decoder(bytes: &[u8]) -> Decoder<Cursor<&[u8]>> {
        Decoder::new(Cursor::new(bytes), Limits::new(128, 64, 64, 16))
    }

    #[test]
    fn hello_bytes_match() {
        let mut encoder = Encoder::new(Vec::new());
        Hello {
            host_pid: 0x0102_0304_0506_0708,
        }
        .encode(&mut encoder)
        .unwrap();
        assert_eq!(
            encoder.into_inner(),
            [0x48, 0x43, 0x4b, 0x48, 1, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1,]
        );
    }

    #[test]
    fn request_round_trip() {
        let request = Request {
            operation: Operation::ObjectWriteAt,
            flags: 7,
            stream: 11,
            offset: 13,
            length: 17,
            name_size: 19,
        };
        let mut encoder = Encoder::new(Vec::new());
        request.encode(&mut encoder).unwrap();
        let encoded = encoder.into_inner();
        assert_eq!(encoded.len(), 48);
        assert_eq!(Request::decode(&mut decoder(&encoded)).unwrap(), request);
    }

    #[test]
    fn reply_and_digest() {
        let reply = Reply {
            status: ReplyStatus::Already,
            value: 29,
            length: 31,
        };
        let mut reply_encoder = Encoder::new(Vec::new());
        reply.encode(&mut reply_encoder).unwrap();
        let reply_bytes = reply_encoder.into_inner();
        assert_eq!(reply_bytes.len(), 32);
        assert_eq!(Reply::decode(&mut decoder(&reply_bytes)).unwrap(), reply);

        let digest = Digest {
            hash: 37,
            files: 41,
            bytes: 43,
        };
        let mut digest_encoder = Encoder::new(Vec::new());
        digest.encode(&mut digest_encoder).unwrap();
        let digest_bytes = digest_encoder.into_inner();
        assert_eq!(digest_bytes.len(), 24);
        assert_eq!(Digest::decode(&mut decoder(&digest_bytes)).unwrap(), digest);
    }

    #[test]
    fn wrong_magic_abi() {
        let mut encoder = Encoder::new(Vec::new());
        Hello { host_pid: 1 }.encode(&mut encoder).unwrap();
        let mut bad_magic = encoder.into_inner();
        bad_magic[0] = 0;
        assert!(matches!(
            Hello::decode(&mut decoder(&bad_magic)),
            Err(WireError::Magic { .. })
        ));

        let mut bad_abi = bad_magic.clone();
        bad_abi[0..4].copy_from_slice(&MAGIC_HELLO.to_le_bytes());
        bad_abi[4..8].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(
            Hello::decode(&mut decoder(&bad_abi)),
            Err(WireError::Abi { .. })
        ));

        let request = Request {
            operation: Operation::ObjectTell,
            flags: 0,
            stream: 1,
            offset: 0,
            length: 0,
            name_size: 0,
        };
        let mut request_encoder = Encoder::new(Vec::new());
        request.encode(&mut request_encoder).unwrap();
        let request_bytes = request_encoder.into_inner();

        let mut bad_operation = request_bytes.clone();
        bad_operation[8..12].copy_from_slice(&99_u32.to_le_bytes());
        assert!(matches!(
            Request::decode(&mut decoder(&bad_operation)),
            Err(WireError::Operation(99))
        ));

        let mut bad_reserved = request_bytes;
        bad_reserved[44..48].copy_from_slice(&1_u32.to_le_bytes());
        assert!(matches!(
            Request::decode(&mut decoder(&bad_reserved)),
            Err(WireError::Reserved(1))
        ));

        let reply = Reply {
            status: ReplyStatus::Ok,
            value: 0,
            length: 0,
        };
        let mut reply_encoder = Encoder::new(Vec::new());
        reply.encode(&mut reply_encoder).unwrap();
        let mut bad_status = reply_encoder.into_inner();
        bad_status[8..12].copy_from_slice(&2_i32.to_le_bytes());
        assert!(matches!(
            Reply::decode(&mut decoder(&bad_status)),
            Err(WireError::Status(2))
        ));
    }

    #[test]
    fn names_and_payloads() {
        let request = Request {
            operation: Operation::Commit,
            flags: 0,
            stream: 0,
            offset: 0,
            length: PAYLOAD_MAX as u64 + 1,
            name_size: 0,
        };
        assert!(matches!(
            request.encode(&mut Encoder::new(Vec::new())),
            Err(WireError::PayloadTooLarge(_))
        ));
    }
}

#[cfg(test)]
mod image_test;
