//! Transferable bounded byte-I/O mechanisms.

#![forbid(unsafe_code)]

use std::io::{self, Read};

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
}
