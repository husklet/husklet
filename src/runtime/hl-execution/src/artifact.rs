use std::{error::Error, fmt};

use hl_isa::GuestArchitecture;

use crate::{RelocationError, RelocationRecord, RelocationTable};

pub const MAX_CODE_BYTES: usize = 1 << 30;
pub const MAX_RELOCATIONS: usize = 1 << 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArenaRequest {
    capacity: usize,
    alignment: usize,
    dual_alias: bool,
}

impl ArenaRequest {
    pub fn new(capacity: usize, alignment: usize, dual_alias: bool) -> Result<Self, PublicationError> {
        if capacity == 0 || capacity > MAX_CODE_BYTES {
            return Err(PublicationError::InvalidArena);
        }
        if alignment == 0 || !alignment.is_power_of_two() || alignment > 2 * 1024 * 1024 {
            return Err(PublicationError::InvalidArena);
        }
        Ok(Self {
            capacity,
            alignment,
            dual_alias,
        })
    }

    pub const fn capacity(self) -> usize {
        self.capacity
    }
    pub const fn alignment(self) -> usize {
        self.alignment
    }
    pub const fn dual_alias(self) -> bool {
        self.dual_alias
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeArtifact {
    pub architecture: GuestArchitecture,
    pub identity: u64,
    pub bytes: Vec<u8>,
    pub relocations: Vec<RelocationRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCodeArtifact {
    artifact: CodeArtifact,
    relocations: RelocationTable,
}

impl CodeArtifact {
    pub fn validate(mut self) -> Result<ValidatedCodeArtifact, PublicationError> {
        if self.bytes.is_empty() || self.bytes.len() > MAX_CODE_BYTES {
            return Err(PublicationError::InvalidCode);
        }
        let records = std::mem::take(&mut self.relocations);
        let relocations =
            RelocationTable::new(records, MAX_RELOCATIONS, self.bytes.len()).map_err(PublicationError::Relocation)?;
        Ok(ValidatedCodeArtifact {
            artifact: self,
            relocations,
        })
    }

    pub fn publish<P: CodePublisher>(self, publisher: &mut P) -> Result<Publication, PublicationError<P::Error>> {
        let artifact = self
            .validate()
            .map_err(|error| PublicationError::Validation(Box::new(error)))?;
        let mut prepared = publisher.prepare(&artifact).map_err(PublicationError::Publisher)?;
        match publisher.commit(&mut prepared) {
            Ok(publication) => Ok(publication),
            Err(error) => {
                publisher.rollback(prepared);
                Err(PublicationError::Publisher(error))
            }
        }
    }
}

impl ValidatedCodeArtifact {
    pub const fn architecture(&self) -> GuestArchitecture {
        self.artifact.architecture
    }
    pub const fn identity(&self) -> u64 {
        self.artifact.identity
    }
    pub fn bytes(&self) -> &[u8] {
        &self.artifact.bytes
    }
    pub fn relocations(&self) -> &RelocationTable {
        &self.relocations
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Publication {
    pub identity: u64,
    pub generation: u64,
    pub code_size: usize,
}

pub trait CodePublisher {
    type Prepared;
    type Error;

    fn prepare(&mut self, artifact: &ValidatedCodeArtifact) -> Result<Self::Prepared, Self::Error>;
    fn commit(&mut self, prepared: &mut Self::Prepared) -> Result<Publication, Self::Error>;
    fn rollback(&mut self, prepared: Self::Prepared);
}

#[derive(Debug, Eq, PartialEq)]
pub enum PublicationError<E = std::convert::Infallible> {
    InvalidArena,
    InvalidCode,
    Relocation(RelocationError),
    Validation(Box<PublicationError>),
    Publisher(E),
}

impl<E: fmt::Debug> fmt::Display for PublicationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl<E: fmt::Debug> Error for PublicationError<E> {}
