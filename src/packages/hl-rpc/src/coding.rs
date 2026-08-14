//! Why a message and a frame payload could not be converted.

use crate::frame::Frame;

/// Why a message and a frame payload could not be converted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Coding {
    Malformed(String),
    Oversize(usize),
}

impl std::fmt::Display for Coding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(detail) => write!(formatter, "the message is not what the frame says it is: {detail}"),
            Self::Oversize(length) => write!(
                formatter,
                "the message encodes to {length} bytes, above the {} byte limit",
                Frame::PAYLOAD_LIMIT
            ),
        }
    }
}

impl std::error::Error for Coding {}

/// Encodes a value as a frame payload.
///
/// Refused here rather than at [`Frame::encode`], so the caller learns the size
/// it overshot by instead of only that it did.
///
/// # Errors
/// Returns `Coding::Oversize` when the encoded value exceeds the payload limit,
/// and `Coding::Malformed` when it cannot be serialized.
pub fn payload<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Coding> {
    let bytes = serde_json::to_vec(value).map_err(|error| Coding::Malformed(error.to_string()))?;
    if bytes.len() > Frame::PAYLOAD_LIMIT {
        return Err(Coding::Oversize(bytes.len()));
    }
    Ok(bytes)
}
