use std::ops::Range;

use crate::dynamic::DynamicTable;
use hl_isa::GuestArchitecture;

/// ELF image placement personality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageKind {
    /// `ET_EXEC`: guest-visible addresses remain at their link-time values.
    Executable,
    /// `ET_DYN`: guest-visible addresses receive a placement bias.
    PositionIndependent,
}

/// Validated byte interval in the source image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileRegion {
    offset: u64,
    size: u64,
}

impl FileRegion {
    pub(crate) const fn new(offset: u64, size: u64) -> Self {
        Self { offset, size }
    }

    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.offset + self.size
    }

    pub(crate) fn as_range(self) -> Range<usize> {
        self.offset as usize..self.end() as usize
    }
}

/// ELF `p_flags`, restricted to the standard load permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentFlags(u8);

impl SegmentFlags {
    pub(crate) const READ: u8 = 4;
    pub(crate) const WRITE: u8 = 2;
    pub(crate) const EXECUTE: u8 = 1;

    pub(crate) const fn new(bits: u8) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn is_readable(self) -> bool {
        self.0 & Self::READ != 0
    }

    #[must_use]
    pub const fn is_writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    #[must_use]
    pub const fn is_executable(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// One validated `PT_LOAD` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadSegment {
    source: FileRegion,
    guest_address: u64,
    memory_size: u64,
    alignment: u64,
    flags: SegmentFlags,
}

impl LoadSegment {
    pub(crate) const fn new(
        source: FileRegion,
        guest_address: u64,
        memory_size: u64,
        alignment: u64,
        flags: SegmentFlags,
    ) -> Self {
        Self {
            source,
            guest_address,
            memory_size,
            alignment,
            flags,
        }
    }

    #[must_use]
    pub const fn source(&self) -> FileRegion {
        self.source
    }

    #[must_use]
    pub const fn guest_address(&self) -> u64 {
        self.guest_address
    }

    #[must_use]
    pub const fn memory_size(&self) -> u64 {
        self.memory_size
    }

    #[must_use]
    pub const fn memory_end(&self) -> u64 {
        self.guest_address + self.memory_size
    }

    #[must_use]
    pub const fn alignment(&self) -> u64 {
        self.alignment
    }

    #[must_use]
    pub const fn flags(&self) -> SegmentFlags {
        self.flags
    }

    #[must_use]
    pub const fn zero_fill_size(&self) -> u64 {
        self.memory_size - self.source.size()
    }

    pub(crate) const fn contains_executable_address(&self, address: u64) -> bool {
        self.flags.is_executable() && address >= self.guest_address && address < self.memory_end()
    }
}

/// Validated program-header table and its guest address when derivable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgramHeaderTable {
    source: FileRegion,
    entry_size: u16,
    entry_count: u16,
    guest_address: Option<u64>,
}

impl ProgramHeaderTable {
    pub(crate) const fn new(source: FileRegion, entry_size: u16, entry_count: u16, guest_address: Option<u64>) -> Self {
        Self {
            source,
            entry_size,
            entry_count,
            guest_address,
        }
    }

    #[must_use]
    pub const fn source(self) -> FileRegion {
        self.source
    }

    #[must_use]
    pub const fn entry_size(self) -> u16 {
        self.entry_size
    }

    #[must_use]
    pub const fn entry_count(self) -> u16 {
        self.entry_count
    }

    #[must_use]
    pub const fn guest_address(self) -> Option<u64> {
        self.guest_address
    }
}

/// Bounded, NUL-free interpreter pathname bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterpreterPath(Vec<u8>);

impl InterpreterPath {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Immutable result of validating one ELF image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePlan {
    architecture: GuestArchitecture,
    kind: ImageKind,
    entry: u64,
    link_base: u64,
    image_span: u64,
    program_headers: ProgramHeaderTable,
    segments: Vec<LoadSegment>,
    interpreter: Option<InterpreterPath>,
    tls: Option<TlsTemplate>,
    relro: Option<RelroRegion>,
    dynamic: Option<DynamicTable>,
}

impl ImagePlan {
    pub(crate) const fn new(
        architecture: GuestArchitecture,
        kind: ImageKind,
        entry: u64,
        link_base: u64,
        image_span: u64,
        program_headers: ProgramHeaderTable,
        segments: Vec<LoadSegment>,
        interpreter: Option<InterpreterPath>,
        tls: Option<TlsTemplate>,
        relro: Option<RelroRegion>,
        dynamic: Option<DynamicTable>,
    ) -> Self {
        Self {
            architecture,
            kind,
            entry,
            link_base,
            image_span,
            program_headers,
            segments,
            interpreter,
            tls,
            relro,
            dynamic,
        }
    }

    #[must_use]
    pub const fn architecture(&self) -> GuestArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn kind(&self) -> ImageKind {
        self.kind
    }

    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.entry
    }

    #[must_use]
    pub const fn link_base(&self) -> u64 {
        self.link_base
    }

    #[must_use]
    pub const fn image_span(&self) -> u64 {
        self.image_span
    }

    #[must_use]
    pub const fn program_headers(&self) -> ProgramHeaderTable {
        self.program_headers
    }

    #[must_use]
    pub fn segments(&self) -> &[LoadSegment] {
        &self.segments
    }

    #[must_use]
    pub const fn interpreter(&self) -> Option<&InterpreterPath> {
        self.interpreter.as_ref()
    }

    #[must_use]
    pub const fn tls(&self) -> Option<&TlsTemplate> {
        self.tls.as_ref()
    }

    #[must_use]
    pub const fn relro(&self) -> Option<RelroRegion> {
        self.relro
    }

    #[must_use]
    pub const fn dynamic(&self) -> Option<&DynamicTable> {
        self.dynamic.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelocationWrite {
    pub address: u64,
    pub value: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelroRegion {
    start: u64,
    size: u64,
}

impl RelroRegion {
    pub(crate) const fn new(start: u64, size: u64) -> Self {
        Self { start, size }
    }

    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.start + self.size
    }

    pub(crate) fn validate(self, segments: &[LoadSegment]) -> bool {
        self.size != 0
            && segments
                .iter()
                .any(|segment| self.start >= segment.guest_address() && self.end() <= segment.memory_end())
    }
}

/// Immutable initialization image described by one validated `PT_TLS`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsTemplate {
    link_address: u64,
    initialized: Vec<u8>,
    memory_size: u64,
    alignment: u64,
}

impl TlsTemplate {
    pub(crate) const fn new(link_address: u64, initialized: Vec<u8>, memory_size: u64, alignment: u64) -> Self {
        Self {
            link_address,
            initialized,
            memory_size,
            alignment,
        }
    }

    #[must_use]
    pub const fn link_address(&self) -> u64 {
        self.link_address
    }

    #[must_use]
    pub fn initialized(&self) -> &[u8] {
        &self.initialized
    }

    #[must_use]
    pub const fn memory_size(&self) -> u64 {
        self.memory_size
    }

    #[must_use]
    pub const fn zero_fill_size(&self) -> u64 {
        self.memory_size - self.initialized.len() as u64
    }

    #[must_use]
    pub const fn alignment(&self) -> u64 {
        self.alignment
    }
}
