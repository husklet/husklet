use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageLimits {
    pub max_image_bytes: usize,
    pub max_program_headers: u16,
    pub max_load_segments: u16,
    pub max_interpreter_bytes: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_image_bytes: 512 * 1024 * 1024,
            max_program_headers: 256,
            max_load_segments: 128,
            max_interpreter_bytes: 4096,
        }
    }
}

/// Failure to turn untrusted ELF bytes into an immutable image plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectError {
    EmptyImage,
    ImageTooLarge,
    TruncatedHeader,
    InvalidMagic,
    UnsupportedClass,
    UnsupportedByteOrder,
    UnsupportedVersion,
    UnsupportedAbi,
    UnsupportedImageKind,
    WrongArchitecture,
    InvalidHeaderSize,
    InvalidProgramHeaderSize,
    MissingProgramHeaders,
    TooManyProgramHeaders,
    TruncatedProgramHeaders,
    TooManyLoadSegments,
    MissingLoadSegment,
    InvalidSegmentFlags,
    SegmentFileLargerThanMemory,
    SegmentOutsideImage,
    InvalidSegmentAlignment,
    AddressOverflow,
    InvalidImageSpan,
    MisalignedEntry,
    EntryOutsideExecutableSegment,
    MultipleInterpreters,
    EmptyInterpreter,
    InterpreterTooLong,
    UnterminatedInterpreter,
    EmbeddedInterpreterNul,
    InvalidProgramHeaderAddress,
    MultipleTlsSegments,
    TlsFileLargerThanMemory,
    TlsOutsideImage,
    InvalidTlsAlignment,
    TlsAddressOverflow,
    MultipleRelroSegments,
    InvalidRelro,
    MultipleDynamicSegments,
    InvalidDynamicTable,
    UnterminatedDynamicTable,
    DuplicateDynamicTag,
}

impl fmt::Display for InspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid ELF image: {self:?}")
    }
}

impl Error for InspectError {}
