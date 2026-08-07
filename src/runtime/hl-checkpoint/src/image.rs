//! Bounded, pointer-free checkpoint image framing.

use std::fmt;

use crate::{CheckpointSink, CheckpointSource, PortError};

const IMAGE_MAGIC: u64 = 0x3147_4d49_4b43_4c48;
const FOOTER_MAGIC: u64 = 0x3144_4e45_4b43_4c48;
const IMAGE_VERSION: u32 = 1;
const HEADER_SIZE: usize = 40;
const ENTRY_SIZE: usize = 32;
const FOOTER_SIZE: usize = 16;

/// Stable domain-defined section identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SectionKind(u32);

impl SectionKind {
    pub fn new(value: u32) -> Result<Self, ImageError> {
        if value == 0 {
            return Err(ImageError::InvalidSectionKind);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Explicit checkpoint resource limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageLimits {
    pub sections: usize,
    pub section_bytes: usize,
    pub image_bytes: usize,
}

impl ImageLimits {
    #[must_use]
    pub const fn new(sections: usize, section_bytes: usize, image_bytes: usize) -> Self {
        Self {
            sections,
            section_bytes,
            image_bytes,
        }
    }
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self::new(256, 4 * 1024 * 1024, 64 * 1024 * 1024)
    }
}

/// One validated, owned image section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    kind: SectionKind,
    version: u32,
    bytes: Vec<u8>,
}

impl Section {
    #[must_use]
    pub fn new(kind: SectionKind, version: u32, bytes: Vec<u8>) -> Self {
        Self { kind, version, bytes }
    }

    #[must_use]
    pub const fn kind(&self) -> SectionKind {
        self.kind
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn validate_order(previous: Option<&Self>, kind: SectionKind) -> Result<(), ImageError> {
        if previous.is_some_and(|section| section.kind >= kind) {
            return Err(ImageError::DuplicateOrUnorderedSection);
        }
        Ok(())
    }
}

/// Fully validated image safe to offer to restore code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointImage {
    sections: Vec<Section>,
    digest: [u8; 32],
}

impl CheckpointImage {
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    #[must_use]
    pub fn section(&self, kind: SectionKind) -> Option<&Section> {
        self.sections.iter().find(|section| section.kind == kind)
    }

    /// SHA-256 over the exact validated checkpoint image bytes.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// Canonical image bytes prepared before transactional publication.
pub struct PreparedImage {
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl PreparedImage {
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn publish<S: CheckpointSink>(&self, sink: &mut S) -> Result<(), ImageError> {
        sink.begin(self.bytes.len())?;
        let result =
            CheckpointWriter::write_image(sink, &self.bytes).and_then(|()| sink.commit().map_err(ImageError::Port));
        if result.is_err() {
            sink.abort();
        }
        result
    }
}

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
    SectionTooLarge { length: usize, maximum: usize },
    ImageTooLarge { size: usize, maximum: usize },
    ArithmeticOverflow,
    Truncated { offset: usize, needed: usize, available: usize },
    TrailingBytes,
    Magic,
    Version(u32),
    Reserved(u32),
    ManifestLength,
    Offset { expected: usize, actual: usize },
    Checksum(ChecksumRegion),
    ZeroProgress,
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

/// Single-domain section builder and transactional image publisher.
pub struct CheckpointWriter {
    limits: ImageLimits,
    sections: Vec<Section>,
}

impl CheckpointWriter {
    #[must_use]
    pub const fn new(limits: ImageLimits) -> Self {
        Self {
            limits,
            sections: Vec::new(),
        }
    }

    pub fn push(&mut self, section: Section) -> Result<(), ImageError> {
        if self.sections.len() >= self.limits.sections {
            return Err(ImageError::SectionLimit);
        }
        if section.bytes.len() > self.limits.section_bytes {
            return Err(ImageError::SectionTooLarge {
                length: section.bytes.len(),
                maximum: self.limits.section_bytes,
            });
        }
        if self
            .sections
            .last()
            .is_some_and(|previous| previous.kind >= section.kind)
        {
            return Err(ImageError::DuplicateOrUnorderedSection);
        }
        self.sections.push(section);
        Ok(())
    }

    pub fn publish<S: CheckpointSink>(&self, sink: &mut S) -> Result<(), ImageError> {
        self.prepare()?.publish(sink)
    }

    pub fn prepare(&self) -> Result<PreparedImage, ImageError> {
        let bytes = self.encode()?;
        let digest = Self::digest(&bytes);
        Ok(PreparedImage { bytes, digest })
    }

    fn digest(bytes: &[u8]) -> [u8; 32] {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .try_into()
            .expect("SHA-256 output is 32 bytes")
    }

    fn write_image<S: CheckpointSink>(sink: &mut S, image: &[u8]) -> Result<(), ImageError> {
        let mut offset = 0;
        while offset < image.len() {
            match sink.write(&image[offset..]) {
                Ok(0) => return Err(ImageError::ZeroProgress),
                Ok(count) if count <= image.len() - offset => offset += count,
                Ok(_) => return Err(ImageError::ZeroProgress),
                Err(PortError::Interrupted) => {}
                Err(PortError::WouldBlock) => sink.wait_writable()?,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, ImageError> {
        let manifest_size = self
            .sections
            .len()
            .checked_mul(ENTRY_SIZE)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let payload_size = self.sections.iter().try_fold(0_usize, |size, section| {
            size.checked_add(section.bytes.len())
                .ok_or(ImageError::ArithmeticOverflow)
        })?;
        let total = HEADER_SIZE
            .checked_add(manifest_size)
            .and_then(|size| size.checked_add(payload_size))
            .and_then(|size| size.checked_add(FOOTER_SIZE))
            .ok_or(ImageError::ArithmeticOverflow)?;
        if total > self.limits.image_bytes {
            return Err(ImageError::ImageTooLarge {
                size: total,
                maximum: self.limits.image_bytes,
            });
        }
        let mut manifest = Vec::with_capacity(manifest_size);
        let mut payload_offset = 0_usize;
        for section in &self.sections {
            Bytes::u32(&mut manifest, section.kind.get());
            Bytes::u32(&mut manifest, section.version);
            Bytes::u64(&mut manifest, payload_offset as u64);
            Bytes::u64(&mut manifest, section.bytes.len() as u64);
            Bytes::u64(&mut manifest, Hash::bytes(&section.bytes));
            payload_offset += section.bytes.len();
        }
        let mut image = Vec::with_capacity(total);
        Bytes::u64(&mut image, IMAGE_MAGIC);
        Bytes::u32(&mut image, IMAGE_VERSION);
        Bytes::u32(&mut image, self.sections.len() as u32);
        Bytes::u64(&mut image, manifest_size as u64);
        Bytes::u64(&mut image, payload_size as u64);
        Bytes::u64(&mut image, Hash::bytes(&manifest));
        image.extend_from_slice(&manifest);
        for section in &self.sections {
            image.extend_from_slice(&section.bytes);
        }
        let image_hash = Hash::bytes(&image);
        Bytes::u64(&mut image, FOOTER_MAGIC);
        Bytes::u64(&mut image, image_hash);
        Ok(image)
    }
}

/// Complete-image validator. No section is exposed before all checks pass.
pub struct CheckpointReader {
    limits: ImageLimits,
}

impl CheckpointReader {
    #[must_use]
    pub const fn new(limits: ImageLimits) -> Self {
        Self { limits }
    }

    pub fn read<S: CheckpointSource>(&self, source: &mut S) -> Result<CheckpointImage, ImageError> {
        let size = source.image_size()?;
        if size > self.limits.image_bytes {
            return Err(ImageError::ImageTooLarge {
                size,
                maximum: self.limits.image_bytes,
            });
        }
        let mut image = vec![0_u8; size];
        Self::read_image(source, &mut image)?;
        self.decode(&image)
    }

    fn read_image<S: CheckpointSource>(source: &mut S, image: &mut [u8]) -> Result<(), ImageError> {
        let mut offset = 0;
        while offset < image.len() {
            match source.read(&mut image[offset..]) {
                Ok(0) => {
                    return Err(ImageError::Truncated {
                        offset,
                        needed: image.len(),
                        available: offset,
                    });
                }
                Ok(count) if count <= image.len() - offset => offset += count,
                Ok(_) => return Err(ImageError::ZeroProgress),
                Err(PortError::Interrupted) => {}
                Err(PortError::WouldBlock) => source.wait_readable()?,
                Err(error) => return Err(error.into()),
            }
        }
        let mut probe = [0_u8; 1];
        if source.read(&mut probe)? != 0 {
            return Err(ImageError::TrailingBytes);
        }
        Ok(())
    }

    fn decode(&self, image: &[u8]) -> Result<CheckpointImage, ImageError> {
        if image.len() < HEADER_SIZE + FOOTER_SIZE {
            return Err(ImageError::Truncated {
                offset: 0,
                needed: HEADER_SIZE + FOOTER_SIZE,
                available: image.len(),
            });
        }
        let footer = image.len() - FOOTER_SIZE;
        if Bytes::read_u64(image, footer)? != FOOTER_MAGIC
            || Bytes::read_u64(image, footer + 8)? != Hash::bytes(&image[..footer])
        {
            return Err(ImageError::Checksum(ChecksumRegion::Footer));
        }
        if Bytes::read_u64(image, 0)? != IMAGE_MAGIC {
            return Err(ImageError::Magic);
        }
        let version = Bytes::read_u32(image, 8)?;
        if version != IMAGE_VERSION {
            return Err(ImageError::Version(version));
        }
        let count = Bytes::read_u32(image, 12)? as usize;
        if count > self.limits.sections {
            return Err(ImageError::SectionLimit);
        }
        let manifest_size = Bytes::usize(image, 16)?;
        let payload_size = Bytes::usize(image, 24)?;
        if manifest_size != count.checked_mul(ENTRY_SIZE).ok_or(ImageError::ArithmeticOverflow)? {
            return Err(ImageError::ManifestLength);
        }
        let manifest_end = HEADER_SIZE
            .checked_add(manifest_size)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let payload_end = manifest_end
            .checked_add(payload_size)
            .ok_or(ImageError::ArithmeticOverflow)?;
        if payload_end != footer {
            return Err(ImageError::ManifestLength);
        }
        let manifest = image.get(HEADER_SIZE..manifest_end).ok_or(ImageError::Truncated {
            offset: HEADER_SIZE,
            needed: manifest_size,
            available: image.len().saturating_sub(HEADER_SIZE),
        })?;
        if Bytes::read_u64(image, 32)? != Hash::bytes(manifest) {
            return Err(ImageError::Checksum(ChecksumRegion::Manifest));
        }
        self.decode_sections(
            manifest,
            &image[manifest_end..payload_end],
            CheckpointWriter::digest(image),
        )
    }

    fn decode_sections(
        &self,
        manifest: &[u8],
        payload: &[u8],
        digest: [u8; 32],
    ) -> Result<CheckpointImage, ImageError> {
        let mut sections = Vec::with_capacity(manifest.len() / ENTRY_SIZE);
        let mut expected_offset = 0_usize;
        for entry in manifest.chunks_exact(ENTRY_SIZE) {
            let kind = SectionKind::new(Bytes::read_u32(entry, 0)?)?;
            Section::validate_order(sections.last(), kind)?;
            let version = Bytes::read_u32(entry, 4)?;
            let offset = Bytes::usize(entry, 8)?;
            let length = Bytes::usize(entry, 16)?;
            if offset != expected_offset {
                return Err(ImageError::Offset {
                    expected: expected_offset,
                    actual: offset,
                });
            }
            if length > self.limits.section_bytes {
                return Err(ImageError::SectionTooLarge {
                    length,
                    maximum: self.limits.section_bytes,
                });
            }
            let end = offset.checked_add(length).ok_or(ImageError::ArithmeticOverflow)?;
            let bytes = payload.get(offset..end).ok_or(ImageError::Truncated {
                offset,
                needed: length,
                available: payload.len().saturating_sub(offset),
            })?;
            if Bytes::read_u64(entry, 24)? != Hash::bytes(bytes) {
                return Err(ImageError::Checksum(ChecksumRegion::Section(kind)));
            }
            sections.push(Section::new(kind, version, bytes.to_vec()));
            expected_offset = end;
        }
        if expected_offset != payload.len() {
            return Err(ImageError::Offset {
                expected: payload.len(),
                actual: expected_offset,
            });
        }
        Ok(CheckpointImage { sections, digest })
    }
}

struct Bytes;

impl Bytes {
    fn u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn read_u32(input: &[u8], offset: usize) -> Result<u32, ImageError> {
        let bytes = input.get(offset..offset + 4).ok_or(ImageError::Truncated {
            offset,
            needed: 4,
            available: input.len().saturating_sub(offset),
        })?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64(input: &[u8], offset: usize) -> Result<u64, ImageError> {
        let bytes = input.get(offset..offset + 8).ok_or(ImageError::Truncated {
            offset,
            needed: 8,
            available: input.len().saturating_sub(offset),
        })?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn usize(input: &[u8], offset: usize) -> Result<usize, ImageError> {
        usize::try_from(Self::read_u64(input, offset)?).map_err(|_| ImageError::ArithmeticOverflow)
    }
}

struct Hash;

impl Hash {
    fn bytes(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }
}
