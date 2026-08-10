use hl_isa::GuestArchitecture;

pub(crate) mod format;
pub(crate) mod view;

use self::format::{
    ELF_CLASS_64, ELF_DATA_LITTLE_ENDIAN, ELF_HEADER_SIZE, ELF_VERSION_CURRENT, GUEST_PAGE_SIZE, IMAGE_SPAN_ALIGNMENT,
    PROGRAM_HEADER_SIZE, PT_DYNAMIC, PT_GNU_RELRO, PT_INTERP, PT_LOAD, PT_PHDR, PT_TLS,
};
use self::view::ElfView;
use crate::dynamic::DynamicTable;
use crate::model::{
    FileRegion, ImageKind, ImagePlan, InterpreterPath, LoadSegment, ProgramHeaderTable, RelroRegion, SegmentFlags,
    TlsTemplate,
};
use crate::{ImageLimits, InspectError};

/// Stateless validator for an ELF64 guest image.
#[derive(Clone, Copy, Debug)]
pub struct ElfInspector {
    architecture: GuestArchitecture,
    limits: ImageLimits,
}

impl ElfInspector {
    #[must_use]
    pub const fn new(architecture: GuestArchitecture, limits: ImageLimits) -> Self {
        Self { architecture, limits }
    }

    pub fn inspect(self, image: &[u8]) -> Result<ImagePlan, InspectError> {
        self.validate_image_bound(image)?;
        let view = ElfView::new(image);
        let header = ElfHeader::parse(
            &image[..ELF_HEADER_SIZE],
            image.len() as u64,
            self.architecture,
            self.limits,
        )?;
        let kind = header.kind;
        let headers = header.headers;
        let mut state = PlanState::new(self.limits.max_load_segments);
        for index in 0..headers.entry_count() {
            let header = view.program_header(headers, index)?;
            state.accept(&view, header, self.limits.max_interpreter_bytes)?;
        }
        let plan = state.finish(self.architecture, kind, header.entry, headers)?;
        Ok(plan)
    }

    fn validate_image_bound(self, image: &[u8]) -> Result<(), InspectError> {
        if image.is_empty() {
            return Err(InspectError::EmptyImage);
        }
        if image.len() > self.limits.max_image_bytes {
            return Err(InspectError::ImageTooLarge);
        }
        if image.len() < ELF_HEADER_SIZE {
            return Err(InspectError::TruncatedHeader);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ElfHeader {
    pub(crate) kind: ImageKind,
    pub(crate) entry: u64,
    pub(crate) headers: ProgramHeaderTable,
}

impl ElfHeader {
    pub(crate) fn parse(
        bytes: &[u8],
        image_length: u64,
        architecture: GuestArchitecture,
        limits: ImageLimits,
    ) -> Result<Self, InspectError> {
        if bytes.len() < ELF_HEADER_SIZE {
            return Err(InspectError::TruncatedHeader);
        }
        let view = ElfView::new(bytes);
        if view.bytes(0, 4) != b"\x7fELF" {
            return Err(InspectError::InvalidMagic);
        }
        if view.byte(4) != ELF_CLASS_64 {
            return Err(InspectError::UnsupportedClass);
        }
        if view.byte(5) != ELF_DATA_LITTLE_ENDIAN {
            return Err(InspectError::UnsupportedByteOrder);
        }
        if view.byte(6) != ELF_VERSION_CURRENT || view.u32(20) != u32::from(ELF_VERSION_CURRENT) {
            return Err(InspectError::UnsupportedVersion);
        }
        if !matches!(view.byte(7), 0 | 3) {
            return Err(InspectError::UnsupportedAbi);
        }
        if view.u16(18) != architecture.elf_machine() {
            return Err(InspectError::WrongArchitecture);
        }
        if view.u16(52) != ELF_HEADER_SIZE as u16 {
            return Err(InspectError::InvalidHeaderSize);
        }
        let kind = match view.u16(16) {
            2 => ImageKind::Executable,
            3 => ImageKind::PositionIndependent,
            _ => return Err(InspectError::UnsupportedImageKind),
        };
        let offset = view.u64(32);
        let entry_size = view.u16(54);
        let entry_count = view.u16(56);
        if entry_size != PROGRAM_HEADER_SIZE {
            return Err(InspectError::InvalidProgramHeaderSize);
        }
        if entry_count == 0 {
            return Err(InspectError::MissingProgramHeaders);
        }
        if entry_count > limits.max_program_headers {
            return Err(InspectError::TooManyProgramHeaders);
        }
        let size = u64::from(entry_size)
            .checked_mul(u64::from(entry_count))
            .ok_or(InspectError::TruncatedProgramHeaders)?;
        if offset.checked_add(size).is_none_or(|end| end > image_length) {
            return Err(InspectError::TruncatedProgramHeaders);
        }
        Ok(Self {
            kind,
            entry: view.u64(24),
            headers: ProgramHeaderTable::new(FileRegion::new(offset, size), entry_size, entry_count, None),
        })
    }
}

struct PlanState {
    max_load_segments: u16,
    segments: Vec<LoadSegment>,
    interpreter: Option<InterpreterPath>,
    explicit_phdr: Option<ProgramHeader>,
    tls: Option<TlsTemplate>,
    relro: Option<RelroRegion>,
    dynamic: Option<DynamicTable>,
}

impl PlanState {
    fn new(max_load_segments: u16) -> Self {
        Self {
            max_load_segments,
            segments: Vec::new(),
            interpreter: None,
            explicit_phdr: None,
            tls: None,
            relro: None,
            dynamic: None,
        }
    }

    fn accept(
        &mut self,
        view: &ElfView<'_>,
        header: ProgramHeader,
        max_interpreter_bytes: usize,
    ) -> Result<(), InspectError> {
        match header.kind {
            PT_LOAD => self.accept_load(view, header),
            PT_INTERP => self.accept_interpreter(view, header, max_interpreter_bytes),
            PT_PHDR => self.accept_program_headers(header),
            PT_TLS => self.accept_tls(view, header),
            PT_GNU_RELRO => self.accept_relro(header),
            PT_DYNAMIC => self.accept_dynamic(view, header),
            _ => Ok(()),
        }
    }

    fn accept_dynamic(&mut self, view: &ElfView<'_>, header: ProgramHeader) -> Result<(), InspectError> {
        if self.dynamic.is_some() {
            return Err(InspectError::MultipleDynamicSegments);
        }
        self.dynamic = Some(DynamicTable::parse(
            view.image,
            header.offset,
            header.file_size,
            header.virtual_address,
        )?);
        Ok(())
    }

    fn accept_relro(&mut self, header: ProgramHeader) -> Result<(), InspectError> {
        if self.relro.is_some() {
            return Err(InspectError::MultipleRelroSegments);
        }
        header
            .virtual_address
            .checked_add(header.memory_size)
            .ok_or(InspectError::InvalidRelro)?;
        self.relro = Some(RelroRegion::new(header.virtual_address, header.memory_size));
        Ok(())
    }

    fn accept_tls(&mut self, view: &ElfView<'_>, header: ProgramHeader) -> Result<(), InspectError> {
        if self.tls.is_some() {
            return Err(InspectError::MultipleTlsSegments);
        }
        if header.file_size > header.memory_size {
            return Err(InspectError::TlsFileLargerThanMemory);
        }
        let region = view.checked_region(header.offset, header.file_size, InspectError::TlsOutsideImage)?;
        header
            .virtual_address
            .checked_add(header.memory_size)
            .ok_or(InspectError::TlsAddressOverflow)?;
        let alignment = header.alignment.max(1);
        if !alignment.is_power_of_two() || header.virtual_address % alignment != header.offset % alignment {
            return Err(InspectError::InvalidTlsAlignment);
        }
        self.tls = Some(TlsTemplate::new(
            header.virtual_address,
            view.image[region.as_range()].to_vec(),
            header.memory_size,
            alignment,
        ));
        Ok(())
    }

    fn accept_load(&mut self, view: &ElfView<'_>, header: ProgramHeader) -> Result<(), InspectError> {
        if self.segments.len() >= usize::from(self.max_load_segments) {
            return Err(InspectError::TooManyLoadSegments);
        }
        header.validate_load(view.image.len() as u64)?;
        // `p_offset` identifies file bytes, not the anonymous BSS extent.  GNU
        // linkers legitimately place a pure-BSS segment at its aligned logical
        // file position beyond EOF.  With `p_filesz == 0` there is no source
        // region to bound against the file; address/alignment and memory bounds
        // are still validated below.
        self.segments.push(LoadSegment::new(
            FileRegion::new(header.offset, header.file_size),
            header.virtual_address,
            header.memory_size,
            header.alignment,
            SegmentFlags::new(header.flags as u8),
        ));
        Ok(())
    }

    fn accept_program_headers(&mut self, header: ProgramHeader) -> Result<(), InspectError> {
        if self.explicit_phdr.is_some() {
            return Err(InspectError::InvalidProgramHeaderAddress);
        }
        header
            .virtual_address
            .checked_add(header.memory_size)
            .ok_or(InspectError::InvalidProgramHeaderAddress)?;
        self.explicit_phdr = Some(header);
        Ok(())
    }

    fn validate_alignment(header: ProgramHeader) -> Result<(), InspectError> {
        if header.alignment > 1
            && (!header.alignment.is_power_of_two()
                || header.virtual_address % header.alignment != header.offset % header.alignment)
        {
            return Err(InspectError::InvalidSegmentAlignment);
        }
        Ok(())
    }

    fn accept_interpreter(
        &mut self,
        view: &ElfView<'_>,
        header: ProgramHeader,
        max_interpreter_bytes: usize,
    ) -> Result<(), InspectError> {
        if self.interpreter.is_some() {
            return Err(InspectError::MultipleInterpreters);
        }
        let size = usize::try_from(header.file_size).map_err(|_| InspectError::InterpreterTooLong)?;
        let region = view.checked_region(header.offset, header.file_size, InspectError::SegmentOutsideImage)?;
        let bytes = &view.image[region.as_range()];
        header.validate_interpreter(bytes, max_interpreter_bytes)?;
        let path = &bytes[..size - 1];
        self.interpreter = Some(InterpreterPath::new(path));
        Ok(())
    }

    fn finish(
        mut self,
        architecture: GuestArchitecture,
        kind: ImageKind,
        entry: u64,
        headers: ProgramHeaderTable,
    ) -> Result<ImagePlan, InspectError> {
        if self.segments.is_empty() {
            return Err(InspectError::MissingLoadSegment);
        }
        self.segments.sort_by_key(LoadSegment::guest_address);
        self.validate_relro()?;
        if !entry.is_multiple_of(u64::from(architecture.instruction_alignment())) {
            return Err(InspectError::MisalignedEntry);
        }
        if !self
            .segments
            .iter()
            .any(|segment| segment.contains_executable_address(entry))
        {
            return Err(InspectError::EntryOutsideExecutableSegment);
        }
        let link_base = self.segments[0].guest_address() & !(GUEST_PAGE_SIZE - 1);
        let max_address = self
            .segments
            .iter()
            .map(LoadSegment::memory_end)
            .max()
            .ok_or(InspectError::MissingLoadSegment)?;
        let image_span = Self::aligned_span(link_base, max_address)?;
        let derived_phdr =
            Self::program_header_address(&self.segments, headers).ok_or(InspectError::InvalidProgramHeaderAddress)?;
        if let Some(header) = self.explicit_phdr {
            let source = headers.source();
            if header.offset != source.offset()
                || header.file_size != source.size()
                || header.memory_size != source.size()
                || header.virtual_address != derived_phdr
            {
                return Err(InspectError::InvalidProgramHeaderAddress);
            }
        }
        let headers = ProgramHeaderTable::new(
            headers.source(),
            headers.entry_size(),
            headers.entry_count(),
            Some(derived_phdr),
        );
        Ok(ImagePlan::new(
            architecture,
            kind,
            entry,
            link_base,
            image_span,
            headers,
            self.segments,
            self.interpreter,
            self.tls,
            self.relro,
            self.dynamic,
        ))
    }

    fn validate_relro(&self) -> Result<(), InspectError> {
        let Some(relro) = self.relro else {
            return Ok(());
        };
        if relro.validate(&self.segments) {
            Ok(())
        } else {
            Err(InspectError::InvalidRelro)
        }
    }

    fn aligned_span(link_base: u64, max_address: u64) -> Result<u64, InspectError> {
        let length = max_address
            .checked_sub(link_base)
            .ok_or(InspectError::InvalidImageSpan)?;
        if length == 0 {
            return Err(InspectError::InvalidImageSpan);
        }
        length
            .checked_add(IMAGE_SPAN_ALIGNMENT - 1)
            .map(|value| value & !(IMAGE_SPAN_ALIGNMENT - 1))
            .ok_or(InspectError::AddressOverflow)
    }

    fn program_header_address(segments: &[LoadSegment], headers: ProgramHeaderTable) -> Option<u64> {
        let table = headers.source();
        segments.iter().find_map(|segment| {
            let source = segment.source();
            if table.offset() < source.offset() || table.end() > source.end() {
                return None;
            }
            segment.guest_address().checked_add(table.offset() - source.offset())
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ProgramHeader {
    pub(crate) kind: u32,
    pub(crate) flags: u32,
    pub(crate) offset: u64,
    pub(crate) virtual_address: u64,
    pub(crate) file_size: u64,
    pub(crate) memory_size: u64,
    pub(crate) alignment: u64,
}

impl ProgramHeader {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, InspectError> {
        if bytes.len() < usize::from(PROGRAM_HEADER_SIZE) {
            return Err(InspectError::TruncatedProgramHeaders);
        }
        let view = ElfView::new(bytes);
        Ok(Self {
            kind: view.u32(0),
            flags: view.u32(4),
            offset: view.u64(8),
            virtual_address: view.u64(16),
            file_size: view.u64(32),
            memory_size: view.u64(40),
            alignment: view.u64(48),
        })
    }

    pub(crate) fn validate_load(self, image_length: u64) -> Result<(), InspectError> {
        if self.flags & !7 != 0 {
            return Err(InspectError::InvalidSegmentFlags);
        }
        if self.file_size > self.memory_size {
            return Err(InspectError::SegmentFileLargerThanMemory);
        }
        if self.file_size != 0
            && self
                .offset
                .checked_add(self.file_size)
                .is_none_or(|end| end > image_length)
        {
            return Err(InspectError::SegmentOutsideImage);
        }
        self.virtual_address
            .checked_add(self.memory_size)
            .ok_or(InspectError::AddressOverflow)?;
        PlanState::validate_alignment(self)
    }

    pub(crate) fn validate_interpreter(self, bytes: &[u8], maximum: usize) -> Result<(), InspectError> {
        let size = usize::try_from(self.file_size).map_err(|_| InspectError::InterpreterTooLong)?;
        if size == 0 {
            return Err(InspectError::EmptyInterpreter);
        }
        if size > maximum {
            return Err(InspectError::InterpreterTooLong);
        }
        if bytes.len() != size {
            return Err(InspectError::SegmentOutsideImage);
        }
        if bytes.last() != Some(&0) {
            return Err(InspectError::UnterminatedInterpreter);
        }
        let path = &bytes[..size - 1];
        if path.is_empty() {
            return Err(InspectError::EmptyInterpreter);
        }
        if path.contains(&0) {
            return Err(InspectError::EmbeddedInterpreterNul);
        }
        Ok(())
    }
}
