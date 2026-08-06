/// Immutable identity returned when a cached block is ready to execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIdentity {
    code_size: u64,
    generation: u64,
}

impl BlockIdentity {
    /// Creates a live block identity.
    pub fn new(code_size: u64, generation: u64) -> Result<Self, DispatchError> {
        if code_size == 0 || generation == 0 {
            return Err(DispatchError::InvalidIdentity);
        }
        Ok(Self { code_size, generation })
    }

    /// Returns the cached block's guest-code byte count.
    #[must_use]
    pub fn code_size(self) -> u64 {
        self.code_size
    }

    /// Returns the cache generation retaining the block.
    #[must_use]
    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// Result of the native cache's mechanism-only lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheObservation {
    /// No live block exists for the guest address.
    Missing,
    /// An immutable cached block is available.
    Available(BlockIdentity),
    /// The lookup used a stale executable mapping epoch.
    MappingEpochMismatch,
}

/// Rust-owned action selected from one cache observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchDecision {
    /// Decode and translate a bounded block.
    Translate,
    /// Execute an already published block.
    Enter(BlockIdentity),
    /// Refresh the mapping identity before another lookup.
    RetryMappingEpoch,
}

impl From<CacheObservation> for DispatchDecision {
    fn from(observation: CacheObservation) -> Self {
        match observation {
            CacheObservation::Missing => Self::Translate,
            CacheObservation::Available(identity) => Self::Enter(identity),
            CacheObservation::MappingEpochMismatch => Self::RetryMappingEpoch,
        }
    }
}

/// Validated request for one bounded translation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranslationRequest {
    guest: u64,
    mapping_epoch: u64,
    source: SourceRange,
    maximum: u64,
}

impl TranslationRequest {
    /// Validates the executable source identity and emission bound.
    pub fn new(
        guest: u64,
        mapping_epoch: u64,
        source_first: u64,
        source_last: u64,
        maximum: u64,
    ) -> Result<Self, DispatchError> {
        if source_last <= source_first || guest < source_first || guest >= source_last || maximum == 0 {
            return Err(DispatchError::InvalidRequest);
        }
        Ok(Self {
            guest,
            mapping_epoch,
            source: SourceRange {
                first: source_first,
                last: source_last,
            },
            maximum,
        })
    }

    /// Returns the first guest instruction address.
    #[must_use]
    pub fn guest(self) -> u64 {
        self.guest
    }

    /// Returns the executable mapping epoch used for lookup.
    #[must_use]
    pub fn mapping_epoch(self) -> u64 {
        self.mapping_epoch
    }

    /// Returns the half-open decoded source range.
    #[must_use]
    pub fn source(self) -> (u64, u64) {
        (self.source.first, self.source.last)
    }

    /// Returns the maximum permitted emitted byte count.
    #[must_use]
    pub fn maximum(self) -> u64 {
        self.maximum
    }

    /// Validates frontend output before executable publication.
    pub fn validate(self, emission: TranslationEmission) -> Result<TranslationEmission, DispatchError> {
        if emission.used == 0
            || emission.used > self.maximum
            || emission.body_offset >= emission.used
            || emission.provenance_count == 0
            || emission.source != self.source
        {
            return Err(DispatchError::InvalidEmission);
        }
        Ok(emission)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceRange {
    first: u64,
    last: u64,
}

/// Bounded frontend result awaiting native W^X publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranslationEmission {
    used: u64,
    body_offset: u64,
    provenance_count: u32,
    source: SourceRange,
}

impl TranslationEmission {
    /// Describes emitted code and its decoded-source identity.
    #[must_use]
    pub fn new(used: u64, body_offset: u64, provenance_count: u32, source_first: u64, source_last: u64) -> Self {
        Self {
            used,
            body_offset,
            provenance_count,
            source: SourceRange {
                first: source_first,
                last: source_last,
            },
        }
    }

    /// Returns the emitted byte count.
    #[must_use]
    pub fn used(self) -> u64 {
        self.used
    }

    /// Returns the executable body offset within the emission.
    #[must_use]
    pub fn body_offset(self) -> u64 {
        self.body_offset
    }

    /// Returns the number of instruction provenance records.
    #[must_use]
    pub fn provenance_count(self) -> u32 {
        self.provenance_count
    }
}

/// Translation-dispatch validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchError {
    /// Executable block identity has an impossible zero field.
    InvalidIdentity,
    /// Translation request is empty, unbounded, or starts outside its source.
    InvalidRequest,
    /// Frontend output exceeds or does not match its reservation.
    InvalidEmission,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn action_selection() {
        let identity = BlockIdentity::new(48, 7).unwrap();
        assert_eq!(
            DispatchDecision::from(CacheObservation::Missing),
            DispatchDecision::Translate
        );
        assert_eq!(
            DispatchDecision::from(CacheObservation::Available(identity)),
            DispatchDecision::Enter(identity)
        );
        assert_eq!(
            DispatchDecision::from(CacheObservation::MappingEpochMismatch),
            DispatchDecision::RetryMappingEpoch
        );
    }

    #[test]
    fn invalid_request() {
        assert_eq!(
            TranslationRequest::new(0x4000, 1, 0x4000, 0x4000, 8),
            Err(DispatchError::InvalidRequest)
        );
        assert_eq!(
            TranslationRequest::new(0x3fff, 1, 0x4000, 0x4008, 8),
            Err(DispatchError::InvalidRequest)
        );
        assert_eq!(
            TranslationRequest::new(0x4000, 1, 0x4000, 0x4008, 0),
            Err(DispatchError::InvalidRequest)
        );
    }

    #[test]
    fn emission_validation() {
        let request = TranslationRequest::new(0x4000, 3, 0x4000, 0x4008, 16).unwrap();
        let valid = TranslationEmission::new(8, 4, 2, 0x4000, 0x4008);
        assert_eq!(request.validate(valid), Ok(valid));
        for invalid in [
            TranslationEmission::new(17, 0, 1, 0x4000, 0x4008),
            TranslationEmission::new(8, 8, 1, 0x4000, 0x4008),
            TranslationEmission::new(8, 0, 0, 0x4000, 0x4008),
            TranslationEmission::new(8, 0, 1, 0x4000, 0x4009),
        ] {
            assert_eq!(request.validate(invalid), Err(DispatchError::InvalidEmission));
        }
    }
}
