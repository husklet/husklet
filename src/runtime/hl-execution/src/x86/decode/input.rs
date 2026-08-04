use super::MAX_INSTRUCTION_BYTES;
use std::{error::Error as StdError, fmt};

pub(super) struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub(super) fn peek(&self) -> Result<u8, Error> {
        if self.position == MAX_INSTRUCTION_BYTES {
            return Err(Error::TooLong);
        }
        self.bytes.get(self.position).copied().ok_or(Error::Truncated)
    }
    pub(super) fn byte(&mut self) -> Result<u8, Error> {
        let byte = self.peek()?;
        self.position += 1;
        Ok(byte)
    }
    pub(super) fn signed(&mut self, width: u8) -> Result<i64, Error> {
        let mut value = 0_u64;
        for shift in 0..width {
            value |= u64::from(self.byte()?) << (shift * 8);
        }
        Ok(match width {
            1 => (value as i8) as i64,
            2 => (value as i16) as i64,
            4 => (value as i32) as i64,
            8 => value as i64,
            _ => return Err(Error::Unsupported),
        })
    }
    pub(super) const fn position(&self) -> usize {
        self.position
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Fetch,
    Truncated,
    TooLong,
    Unknown,
    Unsupported,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl StdError for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchError;
