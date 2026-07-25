//! One submit frame as a value: the [`SubmitHeader`] plus its encoded IR payload.
//!
//! A `Frame` is what crosses the boundary per submit — `[16-byte header][payload]`. The payload is the
//! opaque encoded protocol byte-stream (produced by `protocol::codec::encode_stream`); the transport does
//! not interpret it, it just moves it. The IO that actually writes/reads a frame over a socket lives in
//! [`super::super::adapter::unix`]; this module owns only the value + its byte layout.

use super::header::SubmitHeader;

/// A complete submit frame: header + the encoded IR payload it prefixes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub header: SubmitHeader,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Build a frame from a payload, stamping the header's `len` to the payload length.
    pub fn new(surface_id: u32, width: u32, height: u32, payload: Vec<u8>) -> Self {
        let header = SubmitHeader {
            surface_id,
            width,
            height,
            len: payload.len() as u32,
        };
        Self { header, payload }
    }

    /// Serialize to the on-wire bytes: the 16-byte header followed by the payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SubmitHeader::SIZE + self.payload.len());
        out.extend_from_slice(&self.header.to_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bytes_are_header_then_payload() {
        let f = Frame::new(1, 2, 3, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(f.header.len, 3);
        let bytes = f.to_bytes();
        assert_eq!(bytes.len(), SubmitHeader::SIZE + 3);
        assert_eq!(&bytes[SubmitHeader::SIZE..], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            SubmitHeader::from_bytes(bytes[..SubmitHeader::SIZE].try_into().unwrap()),
            f.header
        );
    }
}
