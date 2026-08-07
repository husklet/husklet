//! Byte-exact C provider frame format.

use crate::ProviderError;

pub(crate) const HEADER_SIZE: usize = 32;
const MAGIC: u32 = 0x484c_5052;
const VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum FrameKind {
    Request = 3,
    Reply = 4,
    Cancel = 5,
    Subscribe = 9,
    Unsubscribe = 10,
    Event = 11,
}

impl FrameKind {
    pub(crate) const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            3 => Some(Self::Request),
            4 => Some(Self::Reply),
            5 => Some(Self::Cancel),
            9 => Some(Self::Subscribe),
            10 => Some(Self::Unsubscribe),
            11 => Some(Self::Event),
            _ => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub kind: FrameKind,
    pub size: usize,
    pub request: u64,
}

impl Header {
    pub(crate) fn encode(kind: FrameKind, size: usize, request: u64) -> Result<[u8; HEADER_SIZE], ProviderError> {
        let size = u32::try_from(size).map_err(|_| ProviderError::PayloadTooLarge {
            size,
            maximum: u32::MAX as usize,
        })?;
        let mut bytes = [0_u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(kind as u16).to_le_bytes());
        bytes[8..12].copy_from_slice(&size.to_le_bytes());
        bytes[12..20].copy_from_slice(&request.to_le_bytes());
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8; HEADER_SIZE], maximum_payload: usize) -> Result<Self, ProviderError> {
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        if magic != MAGIC {
            return Err(ProviderError::MalformedFrame(crate::FrameFault::Magic(magic)));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != VERSION {
            return Err(ProviderError::UnsupportedVersion(version));
        }
        if bytes[20..32].iter().any(|byte| *byte != 0) {
            return Err(ProviderError::MalformedFrame(crate::FrameFault::Reserved));
        }
        let raw_kind = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        let kind = FrameKind::from_raw(raw_kind).ok_or(ProviderError::UnknownFrame(raw_kind))?;
        let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if size > maximum_payload {
            return Err(ProviderError::PayloadTooLarge {
                size,
                maximum: maximum_payload,
            });
        }
        Ok(Self {
            kind,
            size,
            request: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
        })
    }
}
