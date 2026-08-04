use axum::http::StatusCode;

use crate::api::http::error::{ApiError, ApiResult};

#[derive(Clone)]
pub(in crate::api::http) struct DetachKeys(Vec<u8>);

impl DetachKeys {
    pub(in crate::api::http) fn parse(value: &str) -> ApiResult<Option<Self>> {
        if value.is_empty() {
            return Ok(None);
        }
        let invalid = || {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid detach key sequence {value:?}"),
            )
        };
        let mut bytes = Vec::new();
        for token in value.split(',') {
            let byte = Self::token(token).ok_or_else(&invalid)?;
            bytes.push(byte);
        }
        Ok(Some(Self(bytes)))
    }

    fn token(token: &str) -> Option<u8> {
        token
            .strip_prefix("ctrl-")
            .or_else(|| token.strip_prefix("CTRL-"))
            .map_or_else(|| Self::ascii(token), Self::control)
    }

    fn ascii(token: &str) -> Option<u8> {
        let bytes = token.as_bytes();
        bytes
            .first()
            .copied()
            .filter(|byte| bytes.len() == 1 && byte.is_ascii())
    }

    fn control(token: &str) -> Option<u8> {
        match Self::ascii(token)? {
            control @ b'@'..=b'_' => Some(control & 0x1f),
            control @ b'a'..=b'z' => Some(control - b'a' + 1),
            b'?' => Some(0x7f),
            _ => None,
        }
    }
}

pub(super) struct DetachInput {
    keys: Vec<u8>,
    pending: Vec<u8>,
}

impl DetachInput {
    pub(super) fn new(keys: DetachKeys) -> Self {
        Self {
            keys: keys.0,
            pending: Vec::new(),
        }
    }

    pub(super) fn consume(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let mut forward = Vec::new();
        for byte in bytes {
            self.pending.push(*byte);
            while !self.keys.starts_with(&self.pending) {
                forward.push(self.pending.remove(0));
            }
            if self.pending == self.keys {
                self.pending.clear();
                return (forward, true);
            }
        }
        (forward, false)
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_notation() {
        assert_eq!(DetachKeys::parse("").unwrap().map(|keys| keys.0), None);
        assert_eq!(
            DetachKeys::parse("ctrl-p,ctrl-q").unwrap().map(|keys| keys.0),
            Some(vec![16, 17])
        );
        assert_eq!(
            DetachKeys::parse("ctrl-],x").unwrap().map(|keys| keys.0),
            Some(vec![29, b'x'])
        );
        for invalid in ["ctrl-", "ctrl-aa", "ctrl-!", "é"] {
            assert!(DetachKeys::parse(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn split_sequence() {
        let mut input = DetachInput::new(DetachKeys::parse("ctrl-p,ctrl-q").unwrap().unwrap());
        assert_eq!(input.consume(b"hello\x10"), (b"hello".to_vec(), false));
        assert_eq!(input.consume(b"x\x10"), (vec![16, b'x'], false));
        assert_eq!(input.consume(b"\x11ignored"), (Vec::new(), true));

        let mut input = DetachInput::new(DetachKeys::parse("ctrl-p,ctrl-q").unwrap().unwrap());
        assert_eq!(input.consume(b"tail\x10"), (b"tail".to_vec(), false));
        assert_eq!(input.finish(), vec![16]);
    }
}
