//! The device→host **readback** sub-protocol: an additive request/response that returns buffer bytes over
//! the same connection the submit path uses, without disturbing the frozen submit framing.
//!
//! Disjointness (why this never collides with a submit): a readback REQUEST reuses the identical 16-byte
//! [`SubmitHeader`](super::header::SubmitHeader) shape and the same [`read_frame`](super::super::adapter::unix::read_frame)
//! reader, but stamps the header's `surface_id` with the reserved sentinel [`READBACK_MAGIC`]. A real
//! submit's `surface_id` is a small guest-assigned id and can never be that value, so the server tells the
//! two apart with a single compare and every existing submit stays byte-identical on the wire. The reply is
//! NOT the 1-byte submit ack — it is a length-prefixed byte payload ([`write_readback_response`]) the client
//! reads only because it initiated the readback.
//!
//! The sub-protocol carries its own [`READBACK_VERSION`] in every request so it can evolve independently of
//! the handshake's [`WIRE_VERSION`](crate::protocol::model::command::WIRE_VERSION) (currently 6): a readback
//! change never forces a submit-interop-breaking wire-version bump.
//!
//! [`write_readback_response`]: super::super::adapter::unix::write_readback_response

/// Reserved `SubmitHeader::surface_id` sentinel marking a frame as a readback REQUEST rather than a submit.
/// Guest surface ids are small monotonically-assigned values, so this maximal `u32` is never a real surface.
pub const READBACK_MAGIC: u32 = 0xFFFF_FFFF;

/// Version of the readback sub-protocol, carried in every [`ReadbackRequest`]. Independent of the handshake
/// wire version so readback can evolve without a `WIRE_VERSION` bump (which would break submit interop).
pub const READBACK_VERSION: u32 = 1;

/// Response status byte: the readback succeeded and `len` bytes follow.
pub const READBACK_OK: u8 = 1;
/// Response status byte: the readback failed (unknown/unsupported/out-of-bounds); zero bytes follow.
pub const READBACK_FAIL: u8 = 0;

/// The resource kind a [`ReadbackRequest`] targets. Only buffers are supported today; the field reserves
/// room to add textures without changing the request layout.
pub mod readback_kind {
    /// Read back a GPU buffer's bytes.
    pub const BUFFER: u8 = 0;
}

/// A device→host readback request: "return `len` bytes of resource `id` starting at `offset`". Serialized
/// as the payload of a readback-magic frame. Fixed-size, little-endian, self-versioned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadbackRequest {
    /// The readback sub-protocol version ([`READBACK_VERSION`]); a peer rejects a version it predates.
    pub version: u32,
    /// Which resource kind is being read (see [`readback_kind`]).
    pub kind: u8,
    /// The guest-assigned resource id to read from.
    pub id: u32,
    /// Byte offset into the resource.
    pub offset: u64,
    /// Number of bytes to return.
    pub len: u64,
}

impl ReadbackRequest {
    /// Fixed wire size: `version(4) + kind(1) + id(4) + offset(8) + len(8)`.
    pub const SIZE: usize = 4 + 1 + 4 + 8 + 8;

    /// A buffer-readback request for `len` bytes of buffer `id` at `offset`, stamped with the current
    /// [`READBACK_VERSION`].
    pub fn buffer(id: u32, offset: u64, len: u64) -> Self {
        Self { version: READBACK_VERSION, kind: readback_kind::BUFFER, id, offset, len }
    }

    /// Serialize to the fixed little-endian request bytes.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.version.to_le_bytes());
        b[4] = self.kind;
        b[5..9].copy_from_slice(&self.id.to_le_bytes());
        b[9..17].copy_from_slice(&self.offset.to_le_bytes());
        b[17..25].copy_from_slice(&self.len.to_le_bytes());
        b
    }

    /// Parse the fixed little-endian request bytes. Returns `None` if the slice is too short.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            version: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            kind: b[4],
            id: u32::from_le_bytes(b[5..9].try_into().unwrap()),
            offset: u64::from_le_bytes(b[9..17].try_into().unwrap()),
            len: u64::from_le_bytes(b[17..25].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_request_roundtrips() {
        let req = ReadbackRequest::buffer(7, 16, 64);
        assert_eq!(req.version, READBACK_VERSION);
        assert_eq!(req.kind, readback_kind::BUFFER);
        let bytes = req.to_bytes();
        assert_eq!(bytes.len(), ReadbackRequest::SIZE);
        assert_eq!(ReadbackRequest::from_bytes(&bytes), Some(req));
    }

    #[test]
    fn readback_magic_is_not_a_plausible_surface_id() {
        // The sentinel must be a value the guest never assigns to a real surface.
        assert_eq!(READBACK_MAGIC, u32::MAX);
    }

    #[test]
    fn short_request_bytes_decode_to_none() {
        assert_eq!(ReadbackRequest::from_bytes(&[0u8; 4]), None);
    }
}
