//! Pure Darwin syscall planning, testable without an Apple SDK.

use crate::native_host::HostError;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct KqueueInterest {
    pub read: bool,
    pub write: bool,
    pub edge: bool,
    pub oneshot: bool,
}

impl KqueueInterest {
    pub(super) fn decode(bits: u32) -> Result<Self, HostError> {
        let value = Self {
            read: bits & 1 != 0,
            write: bits & 2 != 0,
            edge: bits & 4 != 0,
            oneshot: bits & 8 != 0,
        };
        (value.read || value.write).then_some(value).ok_or(HostError::Invalid)
    }
}

pub(super) struct DarwinPlan;

impl DarwinPlan {
    pub(super) fn timeout_parts(milliseconds: i32) -> Result<Option<(i64, i64)>, HostError> {
        if milliseconds == -1 {
            return Ok(None);
        }
        if milliseconds < 0 {
            return Err(HostError::Invalid);
        }
        let value = Duration::from_millis(milliseconds as u64);
        Ok(Some((
            i64::try_from(value.as_secs()).map_err(|_| HostError::Invalid)?,
            i64::from(value.subsec_nanos()),
        )))
    }

    pub(super) fn unix_path_length(length: usize) -> Result<u8, HostError> {
        let native = length.checked_add(3).ok_or(HostError::Invalid)?;
        if length > 103 {
            return Err(HostError::Invalid);
        }
        u8::try_from(native).map_err(|_| HostError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kqueue_interest_and() {
        assert_eq!(
            KqueueInterest::decode(1 | 4 | 8).unwrap(),
            KqueueInterest {
                read: true,
                write: false,
                edge: true,
                oneshot: true
            }
        );
        assert_eq!(KqueueInterest::decode(0), Err(HostError::Invalid));
        assert_eq!(DarwinPlan::timeout_parts(-1).unwrap(), None);
        assert_eq!(DarwinPlan::timeout_parts(1_250).unwrap(), Some((1, 250_000_000)));
        assert_eq!(DarwinPlan::timeout_parts(-2), Err(HostError::Invalid));
    }

    #[test]
    fn darwin_path_capacity() {
        assert_eq!(DarwinPlan::unix_path_length(103), Ok(106));
        assert_eq!(DarwinPlan::unix_path_length(104), Err(HostError::Invalid));
    }
}
