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
//! the handshake's [`WIRE_VERSION`](crate::protocol::model::command::WIRE_VERSION): a readback
//! change never forces a submit-interop-breaking wire-version bump.
//!
//! [`write_readback_response`]: super::super::adapter::unix::write_readback_response

/// Reserved `SubmitHeader::surface_id` sentinel marking a frame as a readback REQUEST rather than a submit.
/// Guest surface ids are small monotonically-assigned values, so this maximal `u32` is never a real surface.
pub const READBACK_MAGIC: u32 = 0xFFFF_FFFF;

/// Version of the readback sub-protocol, carried in every [`ReadbackRequest`]. Independent of the handshake
/// wire version so readback can evolve without a `WIRE_VERSION` bump (which would break submit interop).
pub const READBACK_VERSION: u32 = 3;

/// Response status byte: the readback succeeded and `len` bytes follow.
pub const READBACK_OK: u8 = 1;
/// Response status byte: the readback failed (unknown/unsupported/out-of-bounds); zero bytes follow.
pub const READBACK_FAIL: u8 = 0;

/// The operation a [`ReadbackRequest`] targets. Buffer readback and cross-session buffer/texture
/// lifecycle operations share this fixed request layout.
pub mod readback_kind {
    /// Read back a GPU buffer's bytes.
    pub const BUFFER: u8 = 0;
    /// Poll a timeline fence; response is exactly one byte (`0` pending, `1` complete).
    pub const FENCE: u8 = 1;
    /// Wait for a timeline fence for at most `len` nanoseconds.
    pub const FENCE_WAIT: u8 = 2;
    /// Export `id` as a process-global buffer capability. Response: `ExportId` as little-endian u64.
    pub const EXPORT_BUFFER: u8 = 3;
    /// Import `offset` (`ExportId`) at caller-minted local buffer `id`. Response: authoritative bytes.
    pub const IMPORT_BUFFER: u8 = 4;
    /// Exclusively map shared local buffer `id`; response is empty success.
    pub const MAP_BUFFER: u8 = 5;
    /// Complete holder work and release shared local buffer `id`; response is empty success.
    pub const UNMAP_BUFFER: u8 = 6;
    pub const EXPORT_TEXTURE: u8 = 7;
    pub const IMPORT_TEXTURE: u8 = 8;
    pub const MAP_TEXTURE: u8 = 9;
    pub const UNMAP_TEXTURE: u8 = 10;
    pub const EXPORT_SYNC: u8 = 11;
    pub const IMPORT_SYNC: u8 = 12;
    pub const RELEASE_SYNC: u8 = 13;
    pub const SIGNAL_SYNC: u8 = 14;
    pub const WAIT_SYNC: u8 = 15;
    /// Query a shared timeline's current value. Response: little-endian u64.
    pub const QUERY_SYNC: u8 = 16;
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
    /// Authenticated synchronization identity, zero for non-sync requests.
    pub authenticity: u128,
    /// Third synchronization argument (wait timeout), zero for other requests.
    pub arg: u64,
}

impl ReadbackRequest {
    /// Fixed wire size: `version(4) + kind(1) + id(4) + offset(8) + len(8)`.
    pub const SIZE: usize = 4 + 1 + 4 + 8 + 8 + 16 + 8;

    /// A buffer-readback request for `len` bytes of buffer `id` at `offset`, stamped with the current
    /// [`READBACK_VERSION`].
    pub fn buffer(id: u32, offset: u64, len: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::BUFFER,
            id,
            offset,
            len,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn fence(id: u32, value: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::FENCE,
            id,
            offset: value,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn fence_wait(id: u32, value: u64, timeout_ns: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::FENCE_WAIT,
            id,
            offset: value,
            len: timeout_ns,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn export_buffer(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::EXPORT_BUFFER,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn import_buffer(id: u32, export: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::IMPORT_BUFFER,
            id,
            offset: export,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn export_texture(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::EXPORT_TEXTURE,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn import_texture(id: u32, export: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::IMPORT_TEXTURE,
            id,
            offset: export,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn map_texture(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::MAP_TEXTURE,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn unmap_texture(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::UNMAP_TEXTURE,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn map_buffer(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::MAP_BUFFER,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn unmap_buffer(id: u32) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::UNMAP_BUFFER,
            id,
            offset: 0,
            len: 0,
            authenticity: 0,
            arg: 0,
        }
    }

    pub fn export_sync(initial: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind: readback_kind::EXPORT_SYNC,
            id: 0,
            offset: 0,
            len: initial,
            authenticity: 0,
            arg: 0,
        }
    }

    fn sync(kind: u8, export: crate::SyncExportId, value: u64, arg: u64) -> Self {
        Self {
            version: READBACK_VERSION,
            kind,
            id: 0,
            offset: export.serial(),
            len: value,
            authenticity: export.authenticity(),
            arg,
        }
    }

    pub fn import_sync(export: crate::SyncExportId) -> Self {
        Self::sync(readback_kind::IMPORT_SYNC, export, 0, 0)
    }
    pub fn release_sync(export: crate::SyncExportId) -> Self {
        Self::sync(readback_kind::RELEASE_SYNC, export, 0, 0)
    }
    pub fn signal_sync(export: crate::SyncExportId, value: u64) -> Self {
        Self::sync(readback_kind::SIGNAL_SYNC, export, value, 0)
    }
    pub fn wait_sync(export: crate::SyncExportId, value: u64, timeout_ns: u64) -> Self {
        Self::sync(readback_kind::WAIT_SYNC, export, value, timeout_ns)
    }
    pub fn query_sync(export: crate::SyncExportId) -> Self {
        Self::sync(readback_kind::QUERY_SYNC, export, 0, 0)
    }

    pub fn sync_export(&self) -> crate::SyncExportId {
        crate::SyncExportId::from_parts(self.offset, self.authenticity)
    }

    /// Serialize to the fixed little-endian request bytes.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.version.to_le_bytes());
        b[4] = self.kind;
        b[5..9].copy_from_slice(&self.id.to_le_bytes());
        b[9..17].copy_from_slice(&self.offset.to_le_bytes());
        b[17..25].copy_from_slice(&self.len.to_le_bytes());
        b[25..41].copy_from_slice(&self.authenticity.to_le_bytes());
        b[41..49].copy_from_slice(&self.arg.to_le_bytes());
        b
    }

    /// Parse the fixed little-endian request bytes. Returns `None` if the slice is too short.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != Self::SIZE {
            return None;
        }
        Some(Self {
            version: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            kind: b[4],
            id: u32::from_le_bytes(b[5..9].try_into().unwrap()),
            offset: u64::from_le_bytes(b[9..17].try_into().unwrap()),
            len: u64::from_le_bytes(b[17..25].try_into().unwrap()),
            authenticity: u128::from_le_bytes(b[25..41].try_into().unwrap()),
            arg: u64::from_le_bytes(b[41..49].try_into().unwrap()),
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
    fn sharing_requests_roundtrip_without_overloading_lengths() {
        let export = ReadbackRequest::export_buffer(7);
        assert_eq!(
            ReadbackRequest::from_bytes(&export.to_bytes()),
            Some(export)
        );
        assert_eq!((export.id, export.offset, export.len), (7, 0, 0));

        let import = ReadbackRequest::import_buffer(11, 0x1234_5678_9abc_def0);
        assert_eq!(
            ReadbackRequest::from_bytes(&import.to_bytes()),
            Some(import)
        );
        assert_eq!(
            (import.id, import.offset, import.len),
            (11, 0x1234_5678_9abc_def0, 0)
        );

        let texture_export = ReadbackRequest::export_texture(13);
        let texture_import = ReadbackRequest::import_texture(17, 0xfeed_beef);
        assert_eq!(
            ReadbackRequest::from_bytes(&texture_export.to_bytes()),
            Some(texture_export)
        );
        assert_eq!(
            ReadbackRequest::from_bytes(&texture_import.to_bytes()),
            Some(texture_import)
        );
        assert_eq!(
            (texture_import.id, texture_import.offset, texture_import.len),
            (17, 0xfeed_beef, 0)
        );

        for request in [
            ReadbackRequest::map_buffer(11),
            ReadbackRequest::unmap_buffer(11),
            ReadbackRequest::map_texture(11),
            ReadbackRequest::unmap_texture(11),
        ] {
            assert_eq!(
                ReadbackRequest::from_bytes(&request.to_bytes()),
                Some(request)
            );
            assert_eq!((request.id, request.offset, request.len), (11, 0, 0));
        }
    }

    #[test]
    fn readback_magic_is_not_a_plausible_surface_id() {
        // The sentinel must be a value the guest never assigns to a real surface.
        assert_eq!(READBACK_MAGIC, u32::MAX);
    }

    #[test]
    fn synchronization_requests_roundtrip_authenticated_identity() {
        let export = crate::SyncExportId::from_parts(17, 0x1234_5678_9abc_def0_1122_3344_5566_7788);
        for request in [
            ReadbackRequest::export_sync(3),
            ReadbackRequest::import_sync(export),
            ReadbackRequest::release_sync(export),
            ReadbackRequest::signal_sync(export, 9),
            ReadbackRequest::wait_sync(export, 10, 11),
            ReadbackRequest::query_sync(export),
        ] {
            assert_eq!(
                ReadbackRequest::from_bytes(&request.to_bytes()),
                Some(request)
            );
        }
        let wait = ReadbackRequest::wait_sync(export, 10, 11);
        assert_eq!(wait.sync_export(), export);
        assert_eq!((wait.len, wait.arg), (10, 11));
    }

    #[test]
    fn every_noncanonical_request_length_decodes_to_none() {
        for len in 0..=ReadbackRequest::SIZE * 2 {
            if len == ReadbackRequest::SIZE {
                continue;
            }
            assert_eq!(
                ReadbackRequest::from_bytes(&vec![0u8; len]),
                None,
                "request len={len}"
            );
        }
    }
}
