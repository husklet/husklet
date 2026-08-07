//! Checkpoint image rejections and the operands that identify them.

use std::fmt;

use crate::PortError;
use crate::image::SectionKind;

/// Names the checksummed span so a mismatch identifies what was corrupted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumRegion {
    Footer,
    Manifest,
    Section(SectionKind),
}

#[derive(Debug)]
pub enum ImageError {
    Port(PortError),
    InvalidSectionKind,
    DuplicateOrUnorderedSection,
    SectionLimit,
    SectionTooLarge {
        length: usize,
        maximum: usize,
    },
    ImageTooLarge {
        size: usize,
        maximum: usize,
    },
    ArithmeticOverflow,
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    TrailingBytes,
    Magic,
    Version(u32),
    Reserved(u32),
    ManifestLength,
    Offset {
        expected: usize,
        actual: usize,
    },
    Checksum(ChecksumRegion),
    ZeroProgress,
}

impl ImageError {
    pub(crate) const fn truncated(offset: usize, needed: usize, available: usize) -> Self {
        Self::Truncated {
            offset,
            needed,
            available,
        }
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "checkpoint image {self:?}")
    }
}

impl std::error::Error for ImageError {}

impl From<PortError> for ImageError {
    fn from(error: PortError) -> Self {
        Self::Port(error)
    }
}
