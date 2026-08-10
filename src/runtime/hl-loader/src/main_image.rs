use crate::elf::{ElfHeader, ProgramHeader};
use crate::{ImageKind, ImageLimits, InspectError};
use hl_isa::GuestArchitecture;

pub trait ImageReadAt {
    fn length(&self) -> Result<u64, ()>;
    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), ()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainImageMetadata {
    pub architecture: GuestArchitecture,
    pub kind: ImageKind,
    pub link_start: u64,
    pub link_end: u64,
    pub interpreter: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainImageInspectError {
    Read,
    Inspect(InspectError),
}

pub struct MainImageInspector {
    architecture: GuestArchitecture,
    limits: ImageLimits,
}

impl MainImageInspector {
    #[must_use]
    pub const fn new(architecture: GuestArchitecture, limits: ImageLimits) -> Self {
        Self { architecture, limits }
    }

    pub fn inspect(self, source: &impl ImageReadAt) -> Result<MainImageMetadata, MainImageInspectError> {
        let length = source.length().map_err(|()| MainImageInspectError::Read)?;
        if length == 0 {
            return Err(MainImageInspectError::Inspect(InspectError::EmptyImage));
        }
        if length > self.limits.max_image_bytes as u64 {
            return Err(MainImageInspectError::Inspect(InspectError::ImageTooLarge));
        }
        if length < 64 {
            return Err(MainImageInspectError::Inspect(InspectError::TruncatedHeader));
        }
        let mut header = [0; 64];
        source
            .read_exact_at(0, &mut header)
            .map_err(|()| MainImageInspectError::Read)?;
        let parsed = ElfHeader::parse(&header, length, self.architecture, self.limits)
            .map_err(MainImageInspectError::Inspect)?;
        let table = parsed.headers.source().offset();
        let entry_size = parsed.headers.entry_size();
        let entries = parsed.headers.entry_count();
        let mut first = u64::MAX;
        let mut last = 0_u64;
        let mut interpreter = None;
        let mut loads = 0_u16;
        let mut executable = Vec::new();
        for index in 0..entries {
            let offset = table
                .checked_add(u64::from(index) * u64::from(entry_size))
                .ok_or(MainImageInspectError::Inspect(InspectError::TruncatedProgramHeaders))?;
            let mut entry = [0; 56];
            source
                .read_exact_at(offset, &mut entry)
                .map_err(|()| MainImageInspectError::Read)?;
            let program = ProgramHeader::parse(&entry).map_err(MainImageInspectError::Inspect)?;
            match program.kind {
                1 => {
                    loads = loads
                        .checked_add(1)
                        .ok_or(MainImageInspectError::Inspect(InspectError::TooManyLoadSegments))?;
                    if loads > self.limits.max_load_segments {
                        return Err(MainImageInspectError::Inspect(InspectError::TooManyLoadSegments));
                    }
                    program.validate_load(length).map_err(MainImageInspectError::Inspect)?;
                    let address = program.virtual_address;
                    let end = address
                        .checked_add(program.memory_size)
                        .ok_or(MainImageInspectError::Inspect(InspectError::AddressOverflow))?;
                    first = first.min(address);
                    last = last.max(end);
                    if program.flags & 1 != 0 {
                        executable.push((address, end));
                    }
                }
                3 => {
                    if interpreter.is_some() {
                        return Err(MainImageInspectError::Inspect(InspectError::MultipleInterpreters));
                    }
                    let offset = program.offset;
                    let size = usize::try_from(program.file_size)
                        .map_err(|_| MainImageInspectError::Inspect(InspectError::InterpreterTooLong))?;
                    let mut path = vec![0; size];
                    source
                        .read_exact_at(offset, &mut path)
                        .map_err(|()| MainImageInspectError::Read)?;
                    program
                        .validate_interpreter(&path, self.limits.max_interpreter_bytes)
                        .map_err(MainImageInspectError::Inspect)?;
                    path.pop();
                    interpreter = Some(path);
                }
                _ => {}
            }
        }
        if first == u64::MAX {
            return Err(MainImageInspectError::Inspect(InspectError::MissingLoadSegment));
        }
        if !parsed
            .entry
            .is_multiple_of(u64::from(self.architecture.instruction_alignment()))
        {
            return Err(MainImageInspectError::Inspect(InspectError::MisalignedEntry));
        }
        if !executable
            .iter()
            .any(|&(start, end)| parsed.entry >= start && parsed.entry < end)
        {
            return Err(MainImageInspectError::Inspect(
                InspectError::EntryOutsideExecutableSegment,
            ));
        }
        let link_start = first & !0xfff;
        let span = last
            .checked_sub(link_start)
            .and_then(|value| value.checked_add(0xffff))
            .ok_or(MainImageInspectError::Inspect(InspectError::AddressOverflow))?
            & !0xffff;
        if span == 0 {
            return Err(MainImageInspectError::Inspect(InspectError::InvalidImageSpan));
        }
        let link_end = link_start
            .checked_add(span)
            .ok_or(MainImageInspectError::Inspect(InspectError::AddressOverflow))?;
        Ok(MainImageMetadata {
            architecture: self.architecture,
            kind: parsed.kind,
            link_start,
            link_end,
            interpreter,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Sparse {
        prefix: Vec<u8>,
        length: u64,
        read: Cell<usize>,
    }

    impl ImageReadAt for Sparse {
        fn length(&self) -> Result<u64, ()> {
            Ok(self.length)
        }
        fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), ()> {
            let offset = usize::try_from(offset).map_err(|_| ())?;
            let bytes = self.prefix.get(offset..offset + output.len()).ok_or(())?;
            output.copy_from_slice(bytes);
            self.read.set(self.read.get() + output.len());
            Ok(())
        }
    }

    fn put16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn image(kind: u16, interpreter: bool) -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut bytes, 16, kind);
        put16(&mut bytes, 18, GuestArchitecture::Aarch64.elf_machine());
        put32(&mut bytes, 20, 1);
        put64(&mut bytes, 24, 0x40_0100);
        put64(&mut bytes, 32, 64);
        put16(&mut bytes, 52, 64);
        put16(&mut bytes, 54, 56);
        put16(&mut bytes, 56, if interpreter { 2 } else { 1 });
        put32(&mut bytes, 64, 1);
        put32(&mut bytes, 68, 5);
        put64(&mut bytes, 72, 0);
        put64(&mut bytes, 80, 0x40_0000);
        put64(&mut bytes, 88, 0x40_0000);
        put64(&mut bytes, 96, 4096);
        put64(&mut bytes, 104, 4096);
        put64(&mut bytes, 112, 4096);
        if interpreter {
            put32(&mut bytes, 120, 3);
            put64(&mut bytes, 128, 256);
            put64(&mut bytes, 152, 7);
            put64(&mut bytes, 160, 7);
            bytes[256..263].copy_from_slice(b"/ld.so\0");
        }
        bytes
    }

    #[test]
    fn projection_agrees_with_authoritative_inspector() {
        for (kind, interpreter) in [(2, false), (3, false), (3, true)] {
            let bytes = image(kind, interpreter);
            let source = Sparse {
                prefix: bytes.clone(),
                length: bytes.len() as u64,
                read: Cell::new(0),
            };
            let metadata = MainImageInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
                .inspect(&source)
                .unwrap();
            let plan = crate::ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
                .inspect(&bytes)
                .unwrap();
            assert_eq!(metadata.kind, plan.kind());
            assert_eq!(metadata.link_start, plan.link_base());
            assert_eq!(metadata.link_end, plan.link_base() + plan.image_span());
            assert_eq!(
                metadata.interpreter.as_deref(),
                plan.interpreter().map(|path| path.as_bytes())
            );
        }
        for mutation in 0..4 {
            let mut bytes = image(2, false);
            match mutation {
                0 => bytes[0] = 0,
                1 => put32(&mut bytes, 68, 8),
                2 => put64(&mut bytes, 112, 3),
                _ => put64(&mut bytes, 24, 0x40_0101),
            }
            let source = Sparse {
                prefix: bytes.clone(),
                length: bytes.len() as u64,
                read: Cell::new(0),
            };
            assert!(
                MainImageInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
                    .inspect(&source)
                    .is_err()
            );
            assert!(
                crate::ElfInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
                    .inspect(&bytes)
                    .is_err()
            );
        }
    }

    #[test]
    fn sparse_large_image_reads_only_bounded_metadata() {
        let mut prefix = vec![0; 4096];
        prefix[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut prefix, 16, 2);
        put16(&mut prefix, 18, GuestArchitecture::Aarch64.elf_machine());
        put32(&mut prefix, 20, 1);
        put64(&mut prefix, 24, 0x40_0100);
        put64(&mut prefix, 32, 64);
        put16(&mut prefix, 52, 64);
        put16(&mut prefix, 54, 56);
        put16(&mut prefix, 56, 1);
        put32(&mut prefix, 64, 1);
        put32(&mut prefix, 68, 5);
        put64(&mut prefix, 80, 0x40_0000);
        put64(&mut prefix, 88, 0x40_0000);
        put64(&mut prefix, 96, 4096);
        put64(&mut prefix, 104, 4096);
        put64(&mut prefix, 112, 4096);
        let source = Sparse {
            prefix,
            length: 512 * 1024 * 1024,
            read: Cell::new(0),
        };
        let plan = MainImageInspector::new(GuestArchitecture::Aarch64, ImageLimits::default())
            .inspect(&source)
            .unwrap();
        assert_eq!((plan.link_start, plan.link_end), (0x40_0000, 0x41_0000));
        assert_eq!(source.read.get(), 64 + 56);
    }
}
