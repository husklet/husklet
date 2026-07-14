//! The transport-private per-frame framing: the 16-byte little-endian submit header + the 1-byte ack.
//!
//! This framing is deliberately NOT routed through `protocol::codec` — it is the transport's own wire and
//! must stay byte-identical to the shipped `gl_shim.c` `exec_stream` reader / the host executor, so an
//! existing guest and host interoperate. Ported from `hl-shim`'s `transport.rs`.

use super::abi::Surface;

/// Per-frame execution-response byte the host executor writes after replaying a submit. v1 of the exec
/// response protocol: a single status byte. [`ACK_OK`] means the frame replayed and its render is
/// committed; any other value — notably [`ACK_FAIL`] — means the host rejected or failed the frame and the
/// guest must NOT treat it as presented.
pub const ACK_OK: u8 = 1;
/// The host executor's documented failure acknowledgement (replay error / missing surface).
pub const ACK_FAIL: u8 = 0;

/// The fixed 16-byte submit header: `[surface.id, surface.width, surface.height, payload_len]`, each a
/// little-endian `u32`, exactly as `gl_shim.c` writes it and the host executor reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SubmitHeader {
    pub surface_id: u32,
    pub width: u32,
    pub height: u32,
    /// Length in bytes of the encoded IR payload that follows this header on the wire.
    pub len: u32,
}

impl SubmitHeader {
    /// The wire size of the header in bytes.
    pub const SIZE: usize = 16;

    /// Build the header for a frame targeting `surface` with a `len`-byte encoded payload.
    pub fn for_frame(surface: &Surface, len: u32) -> Self {
        Self { surface_id: surface.id, width: surface.width, height: surface.height, len }
    }

    /// Serialize to the 16 little-endian bytes on the wire.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut hdr = [0u8; Self::SIZE];
        hdr[0..4].copy_from_slice(&self.surface_id.to_le_bytes());
        hdr[4..8].copy_from_slice(&self.width.to_le_bytes());
        hdr[8..12].copy_from_slice(&self.height.to_le_bytes());
        hdr[12..16].copy_from_slice(&self.len.to_le_bytes());
        hdr
    }

    /// Parse the 16 little-endian header bytes read off the wire.
    pub fn from_bytes(hdr: &[u8; Self::SIZE]) -> Self {
        Self {
            surface_id: u32::from_le_bytes(hdr[0..4].try_into().unwrap()),
            width: u32::from_le_bytes(hdr[4..8].try_into().unwrap()),
            height: u32::from_le_bytes(hdr[8..12].try_into().unwrap()),
            len: u32::from_le_bytes(hdr[12..16].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrips_and_matches_shipped_layout() {
        let surf = Surface { id: 42, width: 640, height: 480, stride: 2560, fd: -1, generation: 0 };
        let h = SubmitHeader::for_frame(&surf, 7);
        let bytes = h.to_bytes();
        // Byte-identical to the shipped gl_shim.c layout: [id, w, h, len] as LE u32s.
        assert_eq!(&bytes[0..4], &42u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &640u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &480u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &7u32.to_le_bytes());
        assert_eq!(SubmitHeader::from_bytes(&bytes), h);
    }
}
