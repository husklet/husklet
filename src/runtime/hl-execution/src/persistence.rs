use std::{error::Error, fmt};

use hl_isa::GuestArchitecture;

use crate::{ArtifactDigest, DIGEST_SEED};

pub const AARCH64_CACHE_ABI: u64 = 0x4136_3450_4341_3032;
pub const X86_64_CACHE_ABI: u64 = 0x5838_3650_4341_3031;
const MAGIC: u64 = 0x3130_5845_524c_4848;
const VERSION: u64 = 1;
const HEADER_SIZE: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheCompatibility {
    pub format: u64,
    pub translator_abi: u64,
}

impl CacheCompatibility {
    pub const fn is_compatible(self, current: Self) -> bool {
        self.format == current.format && self.translator_abi == current.translator_abi
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactName(String);

impl ArtifactName {
    pub fn new(name: impl Into<String>) -> Result<Self, PersistenceError> {
        let name = name.into();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(PersistenceError::InvalidName);
        }
        Ok(Self(name))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct ArtifactCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ArtifactCursor<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    pub fn take(&mut self, size: usize) -> Result<&'a [u8], PersistenceError> {
        let end = self.offset.checked_add(size).ok_or(PersistenceError::Truncated)?;
        let value = self.bytes.get(self.offset..end).ok_or(PersistenceError::Truncated)?;
        self.offset = end;
        Ok(value)
    }
    pub const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

pub trait ArtifactStore {
    type Error;
    fn load_bounded(&self, name: &ArtifactName, maximum: usize) -> Result<Vec<u8>, Self::Error>;
    fn store_atomic(&self, name: &ArtifactName, bytes: &[u8]) -> Result<(), Self::Error>;
    fn remove(&self, name: &ArtifactName) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEnvelope {
    pub architecture: GuestArchitecture,
    pub translator_abi: u64,
    pub identity: u64,
    pub payload: Vec<u8>,
}

impl CacheEnvelope {
    pub fn encode(&self) -> Result<Vec<u8>, PersistenceError> {
        let payload_size = u64::try_from(self.payload.len()).map_err(|_| PersistenceError::TooLarge)?;
        let total = HEADER_SIZE
            .checked_add(self.payload.len())
            .ok_or(PersistenceError::TooLarge)?;
        let mut bytes = Vec::with_capacity(total);
        for word in [
            MAGIC,
            VERSION,
            self.architecture as u64,
            self.translator_abi,
            self.identity,
            payload_size,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&self.payload);
        let checksum = ArtifactDigest::bytes(DIGEST_SEED, &bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8], maximum: usize) -> Result<Self, PersistenceError> {
        if bytes.len() > maximum || bytes.len() < HEADER_SIZE + 8 {
            return Err(PersistenceError::Truncated);
        }
        let content_len = bytes.len() - 8;
        let expected = u64::from_le_bytes(
            bytes[content_len..]
                .try_into()
                .map_err(|_| PersistenceError::Truncated)?,
        );
        if ArtifactDigest::bytes(DIGEST_SEED, &bytes[..content_len]) != expected {
            return Err(PersistenceError::Checksum);
        }
        let mut cursor = ArtifactCursor::new(&bytes[..content_len]);
        let mut word = || -> Result<u64, PersistenceError> {
            Ok(u64::from_le_bytes(
                cursor.take(8)?.try_into().map_err(|_| PersistenceError::Truncated)?,
            ))
        };
        if word()? != MAGIC {
            return Err(PersistenceError::Magic);
        }
        if word()? != VERSION {
            return Err(PersistenceError::Version);
        }
        let architecture = match word()? {
            1 => GuestArchitecture::Aarch64,
            2 => GuestArchitecture::X86_64,
            _ => return Err(PersistenceError::Architecture),
        };
        let translator_abi = word()?;
        let identity = word()?;
        let payload_size = usize::try_from(word()?).map_err(|_| PersistenceError::TooLarge)?;
        let payload = cursor.take(payload_size)?.to_vec();
        if cursor.remaining() != 0 {
            return Err(PersistenceError::Trailing);
        }
        Ok(Self {
            architecture,
            translator_abi,
            identity,
            payload,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceError {
    InvalidName,
    TooLarge,
    Truncated,
    Checksum,
    Magic,
    Version,
    Architecture,
    Trailing,
}
impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for PersistenceError {}
