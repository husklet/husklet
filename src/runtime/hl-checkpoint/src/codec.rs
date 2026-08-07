//! Transferable bounded binary encoding and decoding.

#![forbid(unsafe_code)]

use std::fmt;
use std::io::{self, Read, Write};

use crate::io::{BoundedReader, Limit};

/// Independent allocation limits applied while decoding untrusted input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub input: Limit,
    pub bytes: usize,
    pub string: usize,
    pub collection: usize,
}

impl Limits {
    #[must_use]
    pub const fn new(input: usize, bytes: usize, string: usize, collection: usize) -> Self {
        Self {
            input: Limit::new(input),
            bytes,
            string,
            collection,
        }
    }
}

/// Encoding failure.
#[derive(Debug)]
pub enum EncodeError {
    Io { offset: u64, error: io::Error },
    LengthOverflow { offset: u64, length: usize },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { offset, error } => write!(formatter, "encoding failed at offset {offset}: {error}"),
            Self::LengthOverflow { offset, length } => {
                write!(formatter, "value length {length} at offset {offset} exceeds u32")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// Rejection of malformed, truncated, or oversized input.
#[derive(Debug)]
pub enum DecodeError {
    Io {
        offset: usize,
        error: io::Error,
    },
    Truncated {
        offset: usize,
        requested: usize,
    },
    InvalidUtf8 {
        offset: usize,
        length: usize,
    },
    LimitExceeded {
        offset: usize,
        kind: LimitKind,
        requested: usize,
        maximum: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { offset, error } => write!(formatter, "decoding failed at offset {offset}: {error}"),
            Self::Truncated { offset, requested } => {
                write!(formatter, "encoded value of {requested} bytes at offset {offset} is truncated")
            }
            Self::InvalidUtf8 { offset, length } => {
                write!(formatter, "encoded string of {length} bytes at offset {offset} is not UTF-8")
            }
            Self::LimitExceeded {
                offset,
                kind,
                requested,
                maximum,
            } => write!(
                formatter,
                "{kind:?} length {requested} at offset {offset} exceeds maximum {maximum}"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Limit category reported without schema-specific policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    Bytes,
    String,
    Collection,
}

/// Explicit little-endian encoder.
#[derive(Debug)]
pub struct Encoder<W> {
    output: W,
    written: u64,
}

impl<W> Encoder<W> {
    #[must_use]
    pub const fn new(output: W) -> Self {
        Self { output, written: 0 }
    }

    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    pub fn into_inner(self) -> W {
        self.output
    }
}

impl<W: Write> Encoder<W> {
    pub fn write_u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.write_all(&[value])
    }

    pub fn write_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.write_all(&value.to_le_bytes())
    }

    pub fn write_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.write_all(&value.to_le_bytes())
    }

    pub fn write_u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.write_all(&value.to_le_bytes())
    }

    pub fn write_i32(&mut self, value: i32) -> Result<(), EncodeError> {
        self.write_all(&value.to_le_bytes())
    }

    pub fn write_bytes(&mut self, value: &[u8]) -> Result<(), EncodeError> {
        let length = u32::try_from(value.len()).map_err(|_| EncodeError::LengthOverflow {
            offset: self.written,
            length: value.len(),
        })?;
        self.write_u32(length)?;
        self.write_all(value)
    }

    pub fn write_string(&mut self, value: &str) -> Result<(), EncodeError> {
        self.write_bytes(value.as_bytes())
    }

    fn write_all(&mut self, value: &[u8]) -> Result<(), EncodeError> {
        let offset = self.written;
        self.output
            .write_all(value)
            .map_err(|error| EncodeError::Io { offset, error })?;
        self.written = self
            .written
            .checked_add(u64::try_from(value.len()).unwrap_or(u64::MAX))
            .ok_or(EncodeError::LengthOverflow {
                offset,
                length: value.len(),
            })?;
        Ok(())
    }
}

/// Decoder whose underlying reader and every allocated field are bounded.
#[derive(Debug)]
pub struct Decoder<R> {
    input: BoundedReader<R>,
    limits: Limits,
}

impl<R> Decoder<R> {
    #[must_use]
    pub const fn new(input: R, limits: Limits) -> Self {
        Self {
            input: BoundedReader::new(input, limits.input),
            limits,
        }
    }

    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.input.consumed()
    }

    pub fn into_inner(self) -> R {
        self.input.into_inner()
    }
}

impl<R: Read> Decoder<R> {
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let mut bytes = [0_u8; 1];
        self.read_exact(&mut bytes)?;
        Ok(bytes[0])
    }

    pub fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let mut bytes = [0_u8; 2];
        self.read_exact(&mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    pub fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let mut bytes = [0_u8; 4];
        self.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let mut bytes = [0_u8; 8];
        self.read_exact(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn read_i32(&mut self) -> Result<i32, DecodeError> {
        let mut bytes = [0_u8; 4];
        self.read_exact(&mut bytes)?;
        Ok(i32::from_le_bytes(bytes))
    }

    pub fn read_bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let length = self.read_length(LimitKind::Bytes, self.limits.bytes)?;
        let mut value = vec![0_u8; length];
        self.read_exact(&mut value)?;
        Ok(value)
    }

    pub fn read_string(&mut self) -> Result<String, DecodeError> {
        let length = self.read_length(LimitKind::String, self.limits.string)?;
        let offset = self.consumed();
        let mut value = vec![0_u8; length];
        self.read_exact(&mut value)?;
        String::from_utf8(value).map_err(|_| DecodeError::InvalidUtf8 { offset, length })
    }

    pub fn read_count(&mut self) -> Result<usize, DecodeError> {
        self.read_length(LimitKind::Collection, self.limits.collection)
    }

    fn read_length(&mut self, kind: LimitKind, maximum: usize) -> Result<usize, DecodeError> {
        let offset = self.consumed();
        let requested = usize::try_from(self.read_u32()?).map_err(|_| DecodeError::LimitExceeded {
            offset,
            kind,
            requested: usize::MAX,
            maximum,
        })?;
        if requested > maximum {
            return Err(DecodeError::LimitExceeded {
                offset,
                kind,
                requested,
                maximum,
            });
        }
        Ok(requested)
    }

    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), DecodeError> {
        let offset = self.input.consumed();
        let requested = output.len();
        self.input.read_exact(output).map_err(|error| {
            if matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData) {
                DecodeError::Truncated { offset, requested }
            } else {
                DecodeError::Io { offset, error }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn limits() -> Limits {
        Limits::new(1024, 32, 32, 8)
    }

    #[test]
    fn integer_endianness() {
        let mut encoder = Encoder::new(Vec::new());
        encoder.write_u16(0x1234).unwrap();
        encoder.write_u32(0x89ab_cdef).unwrap();
        encoder.write_u64(0x0123_4567_89ab_cdef).unwrap();
        encoder.write_i32(-2).unwrap();
        let bytes = encoder.into_inner();
        assert_eq!(
            bytes,
            [
                0x34, 0x12, 0xef, 0xcd, 0xab, 0x89, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0xfe, 0xff, 0xff,
                0xff,
            ]
        );
    }

    #[test]
    fn exact_roundtrip() {
        let mut encoder = Encoder::new(Vec::new());
        encoder.write_u8(7).unwrap();
        encoder.write_string("engine").unwrap();
        encoder.write_bytes(&[1, 2, 3]).unwrap();
        let encoded = encoder.into_inner();

        let mut decoder = Decoder::new(Cursor::new(&encoded), limits());
        assert_eq!(decoder.read_u8().unwrap(), 7);
        assert_eq!(decoder.read_string().unwrap(), "engine");
        assert_eq!(decoder.read_bytes().unwrap(), [1, 2, 3]);
        assert_eq!(decoder.consumed(), encoded.len());
    }

    #[test]
    fn length_preflight() {
        let encoded = 33_u32.to_le_bytes();
        let mut decoder = Decoder::new(Cursor::new(encoded), limits());
        assert!(matches!(
            decoder.read_bytes(),
            Err(DecodeError::LimitExceeded {
                offset: 0,
                kind: LimitKind::Bytes,
                requested: 33,
                maximum: 32,
            })
        ));
        assert_eq!(decoder.consumed(), 4);
    }

    #[test]
    fn text_errors() {
        let mut truncated = Decoder::new(Cursor::new([4, 0, 0, 0, 1, 2]), limits());
        assert!(matches!(
            truncated.read_bytes(),
            Err(DecodeError::Truncated {
                offset: 4,
                requested: 4
            })
        ));

        let mut invalid = Decoder::new(Cursor::new([2, 0, 0, 0, 0xff, 0xff]), limits());
        assert!(matches!(
            invalid.read_string(),
            Err(DecodeError::InvalidUtf8 {
                offset: 4,
                length: 2
            })
        ));
    }
}
