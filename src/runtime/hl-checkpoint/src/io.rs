//! Transferable bounded byte-I/O mechanisms.

#![forbid(unsafe_code)]

use std::fmt;
use std::io::{self, Read, Write};

/// An explicit upper bound for an I/O operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limit {
    bytes: usize,
}

impl Limit {
    #[must_use]
    pub const fn new(bytes: usize) -> Self {
        Self { bytes }
    }

    #[must_use]
    pub const fn bytes(self) -> usize {
        self.bytes
    }
}

/// Failure while reading a bounded stream to completion.
#[derive(Debug)]
pub enum BoundedReadError {
    Io(io::Error),
    LimitExceeded { limit: Limit },
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "bounded read failed: {error}"),
            Self::LimitExceeded { limit } => {
                write!(formatter, "input exceeds {} bytes", limit.bytes())
            }
        }
    }
}

impl std::error::Error for BoundedReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::LimitExceeded { .. } => None,
        }
    }
}

impl From<io::Error> for BoundedReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reader view that cannot consume more than its configured limit.
#[derive(Debug)]
pub struct BoundedReader<R> {
    inner: R,
    limit: Limit,
    consumed: usize,
}

impl<R> BoundedReader<R> {
    #[must_use]
    pub const fn new(inner: R, limit: Limit) -> Self {
        Self {
            inner,
            limit,
            consumed: 0,
        }
    }

    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.consumed
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.limit.bytes.saturating_sub(self.consumed)
    }

    #[must_use]
    pub const fn limit(&self) -> Limit {
        self.limit
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> BoundedReader<R> {
    /// Reads exactly `output.len()` bytes without crossing the limit.
    pub fn read_exact(&mut self, output: &mut [u8]) -> io::Result<()> {
        if output.len() > self.remaining() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bounded reader limit exceeded",
            ));
        }
        self.inner.read_exact(output)?;
        self.consumed += output.len();
        Ok(())
    }

    /// Reads through EOF, rejecting rather than truncating oversized input.
    pub fn read_to_end(mut self) -> Result<Vec<u8>, BoundedReadError> {
        let mut output = Vec::new();
        let remaining = self.remaining();
        self.inner
            .by_ref()
            .take(u64::try_from(remaining).unwrap_or(u64::MAX))
            .read_to_end(&mut output)?;
        self.consumed += output.len();

        let mut probe = [0_u8; 1];
        loop {
            match self.inner.read(&mut probe) {
                Ok(0) => return Ok(output),
                Ok(_) => {
                    return Err(BoundedReadError::LimitExceeded { limit: self.limit });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(BoundedReadError::Io(error)),
            }
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.remaining() == 0 {
            return Ok(0);
        }
        let permitted = output.len().min(self.remaining());
        let count = self.inner.read(&mut output[..permitted])?;
        self.consumed += count;
        Ok(count)
    }
}

/// Failure while copying a bounded stream.
#[derive(Debug)]
pub enum CopyError {
    Read(io::Error),
    Write(io::Error),
    LimitExceeded { limit: Limit },
}

impl fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "copy read failed: {error}"),
            Self::Write(error) => write!(formatter, "copy write failed: {error}"),
            Self::LimitExceeded { limit } => {
                write!(formatter, "copy exceeds {} bytes", limit.bytes())
            }
        }
    }
}

impl std::error::Error for CopyError {}

struct CopyOperation;

impl CopyOperation {
    fn prove_end<R: Read>(reader: &mut R, limit: Limit, copied: usize) -> Result<u64, CopyError> {
        let mut probe = [0_u8; 1];
        loop {
            match reader.read(&mut probe) {
                Ok(0) => return Ok(copied as u64),
                Ok(_) => return Err(CopyError::LimitExceeded { limit }),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(CopyError::Read(error)),
            }
        }
    }

    fn read<R: Read>(reader: &mut R, output: &mut [u8]) -> Result<usize, CopyError> {
        loop {
            match reader.read(output) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(CopyError::Read(error)),
            }
        }
    }
}

/// Copies through EOF while proving the source does not exceed `limit`.
pub fn copy_bounded<R: Read, W: Write>(reader: &mut R, writer: &mut W, limit: Limit) -> Result<u64, CopyError> {
    let mut copied = 0_usize;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining = limit.bytes.saturating_sub(copied);
        if remaining == 0 {
            return CopyOperation::prove_end(reader, limit, copied);
        }
        let requested = remaining.min(buffer.len());
        let count = CopyOperation::read(reader, &mut buffer[..requested])?;
        if count == 0 {
            return Ok(copied as u64);
        }
        writer.write_all(&buffer[..count]).map_err(CopyError::Write)?;
        copied += count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reader_limit() {
        let mut reader = BoundedReader::new(Cursor::new(b"abcdef"), Limit::new(4));
        let mut output = [0_u8; 8];
        assert_eq!(reader.read(&mut output).unwrap(), 4);
        assert_eq!(&output[..4], b"abcd");
        assert_eq!(reader.read(&mut output).unwrap(), 0);
        assert_eq!(reader.consumed(), 4);
    }

    #[test]
    fn exact_preflight() {
        let mut reader = BoundedReader::new(Cursor::new(b"abcdef"), Limit::new(3));
        let mut output = [0_u8; 4];
        assert_eq!(
            reader.read_exact(&mut output).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(reader.consumed(), 0);
        let mut prefix = [0_u8; 3];
        reader.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"abc");
    }

    #[test]
    fn end_read_limit() {
        let exact = BoundedReader::new(Cursor::new(b"abcd"), Limit::new(4)).read_to_end();
        assert_eq!(exact.unwrap(), b"abcd");

        let oversized = BoundedReader::new(Cursor::new(b"abcde"), Limit::new(4)).read_to_end();
        assert!(matches!(oversized, Err(BoundedReadError::LimitExceeded { .. })));
    }

    #[test]
    fn bounded_copy_limit() {
        let mut source = Cursor::new(b"abcd");
        let mut output = Vec::new();
        assert_eq!(copy_bounded(&mut source, &mut output, Limit::new(4)).unwrap(), 4);
        assert_eq!(output, b"abcd");

        let mut oversized = Cursor::new(b"abcde");
        let mut partial = Vec::new();
        assert!(matches!(
            copy_bounded(&mut oversized, &mut partial, Limit::new(4)),
            Err(CopyError::LimitExceeded { .. })
        ));
        assert_eq!(partial, b"abcd");
    }
}
