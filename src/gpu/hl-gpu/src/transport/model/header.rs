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
/// The host executor's UNCLASSIFIED failure acknowledgement (replay error / missing surface).
///
/// Still the value an older host sends for every refusal, and still the value a newer host sends when it
/// has no better classification, so it must keep meaning "refused, reason unstated" forever.
pub const ACK_FAIL: u8 = 0;

// ---- classified refusals ------------------------------------------------------------------------
//
// The acknowledgement is ONE byte and cannot grow. Traffic on this connection is host→guest only — the
// guest reads the host's advertisement and never announces itself (`CommandSink::negotiate`) — so the
// host cannot know whether a guest would understand a longer reply, and lengthening it would
// desynchronise an older guest on the very next frame. Carrying the reason as distinct BYTE VALUES needs
// no layout change and degrades safely in both directions: an older guest treats every non-`ACK_OK`
// value as a refusal already, and a newer guest reads `ACK_FAIL` from an older host exactly as before.
//
// Mixed versions are not hypothetical. A host worker twelve hours older than the guest driver loaded
// against it mis-decoded a wire change on this machine, in one session, and invalidated three
// measurements before the mismatch was noticed. "No old peers exist" is the assumption that holds until
// it does not.
/// The host does not implement the operation at all.
pub const ACK_UNSUPPORTED: u8 = 2;
/// The request exceeded a negotiated or device limit.
pub const ACK_RESOURCE_LIMIT: u8 = 3;
/// The request was malformed or violated an invariant.
pub const ACK_INVALID: u8 = 4;
/// A range in the request fell outside the resource it addressed.
pub const ACK_OUT_OF_BOUNDS: u8 = 5;
/// The request named a resource the host does not have.
pub const ACK_UNKNOWN_ID: u8 = 6;
/// The shader or kernel payload could not be lowered by the host.
///
/// Distinct from [`ACK_INVALID`] because a caller can act on it: the request was well-formed and the
/// resources existed, but the program text uses something this host's shader translation does not
/// implement. Collapsing it into `Invalid` costs a surface that has its own code for exactly this —
/// CUDA's `CUDA_ERROR_INVALID_PTX` — the ability to report it, which is the whole point of carrying a
/// class at all.
pub const ACK_KERNEL: u8 = 7;

/// The host refused because a resource shared across connections is mapped by a different one. A TIMING
/// refusal: the identical frame from the same guest succeeds once the holder unmaps.
///
/// Allocated ABOVE every code that shipped before it, which is what makes it additive — a guest that
/// predates this value has no arm for it and takes `from_ack`'s wildcard to `Unstated`, landing on the
/// same generic refusal it always did. That property is asserted in `an_older_guest_reads_a_newer_code_as_unstated`,
/// not argued: a wildcard doing the right thing stays right only until someone adds an arm above it.
pub const ACK_MAPPED_ELSEWHERE: u8 = 8;

/// Why the host refused a frame, as far as one acknowledgement byte can say.
///
/// This is a REASON CLASS, not an identity: it says what kind of thing was wrong, never which command in
/// the batch was wrong. Carrying the index needs the guest to announce itself first, which this protocol
/// has no channel for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefusalKind {
    /// The host stated no reason — an older host, or a failure with no better classification.
    Unstated,
    Unsupported,
    ResourceLimit,
    Invalid,
    OutOfBounds,
    UnknownId,
    /// The host could not lower the shader/kernel payload.
    Kernel,
    /// A resource shared across connections is mapped by another one. Recoverable BY WAITING, unlike
    /// every other kind here, which are recoverable only by sending something different.
    MappedElsewhere,
}

impl RefusalKind {
    /// Read the class out of a non-`ACK_OK` acknowledgement byte. An unrecognised value is `Unstated`
    /// rather than an error: a future host may classify more finely than this guest understands, and the
    /// refusal is still a refusal.
    pub fn from_ack(ack: u8) -> Self {
        match ack {
            ACK_UNSUPPORTED => Self::Unsupported,
            ACK_RESOURCE_LIMIT => Self::ResourceLimit,
            ACK_INVALID => Self::Invalid,
            ACK_OUT_OF_BOUNDS => Self::OutOfBounds,
            ACK_UNKNOWN_ID => Self::UnknownId,
            ACK_KERNEL => Self::Kernel,
            ACK_MAPPED_ELSEWHERE => Self::MappedElsewhere,
            _ => Self::Unstated,
        }
    }

    pub fn ack(self) -> u8 {
        match self {
            Self::Unstated => ACK_FAIL,
            Self::Unsupported => ACK_UNSUPPORTED,
            Self::ResourceLimit => ACK_RESOURCE_LIMIT,
            Self::Invalid => ACK_INVALID,
            Self::OutOfBounds => ACK_OUT_OF_BOUNDS,
            Self::UnknownId => ACK_UNKNOWN_ID,
            Self::Kernel => ACK_KERNEL,
            Self::MappedElsewhere => ACK_MAPPED_ELSEWHERE,
        }
    }

    /// Classify the typed error the host refused with. Transport, decode and panic failures are NOT
    /// refusals — they mean the connection or the backend is no longer trustworthy — so they stay
    /// `Unstated` and a guest continues to treat them as terminal.
    pub fn for_error(error: &crate::protocol::model::error::GpuError) -> Self {
        use crate::protocol::model::error::GpuError as E;
        match error {
            E::Unsupported(_) => Self::Unsupported,
            E::ResourceLimit(_) => Self::ResourceLimit,
            E::OutOfBounds => Self::OutOfBounds,
            E::UnknownId { .. } | E::DuplicateId { .. } => Self::UnknownId,
            E::Invalid(_)
            | E::BadEnum { .. }
            | E::BadTag(_)
            | E::NonFinite(_)
            | E::NonCanonicalBool(_)
            | E::Utf8
            | E::ShortBuffer
            | E::TrailingBytes => Self::Invalid,
            E::Kernel(_) => Self::Kernel,
            // A TIMING refusal, and now the only kind on this wire that a guest could recover from by
            // waiting rather than by sending something different.
            E::MappedElsewhere { .. } => Self::MappedElsewhere,
            E::Decode(_) | E::Transport(_) | E::Panicked(_) => Self::Unstated,
        }
    }
}

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
        Self {
            surface_id: surface.id,
            width: surface.width,
            height: surface.height,
            len,
        }
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
mod refusal_tests {
    use super::*;
    use crate::protocol::model::error::GpuError;

    /// A class chosen on the host must survive the byte and arrive as the same class on the guest. The
    /// round trip is the whole mechanism; everything either side of it is a lookup table.
    #[test]
    fn a_refusal_class_survives_the_acknowledgement_byte() {
        for (error, expected) in [
            (GpuError::Unsupported("x"), RefusalKind::Unsupported),
            (GpuError::ResourceLimit("x"), RefusalKind::ResourceLimit),
            (GpuError::OutOfBounds, RefusalKind::OutOfBounds),
            (GpuError::Invalid("x"), RefusalKind::Invalid),
            (
                GpuError::UnknownId { kind: "x", id: 1 },
                RefusalKind::UnknownId,
            ),
        ] {
            let sent = RefusalKind::for_error(&error);
            assert_eq!(sent, expected, "host classified {error:?} wrongly");
            assert_eq!(
                RefusalKind::from_ack(sent.ack()),
                expected,
                "class did not survive the byte for {error:?}"
            );
            assert_ne!(sent.ack(), ACK_OK, "a refusal must never encode as success");
        }
    }

    /// A failure that is NOT a refusal stays unstated, so a guest keeps treating it as terminal. Handing
    /// a transport or panic failure an ordinary refusal class would tell an application to carry on
    /// against a connection that is gone.
    #[test]
    fn a_non_refusal_is_never_given_a_refusal_class() {
        for error in [GpuError::Decode("x".into()), GpuError::Panicked("x".into())] {
            assert_eq!(RefusalKind::for_error(&error), RefusalKind::Unstated);
            assert_eq!(RefusalKind::for_error(&error).ack(), ACK_FAIL);
        }
    }

    /// THE CONTRACT, stated over every value the byte can hold: any acknowledgement that is not
    /// `ACK_OK` is a refusal. A reader that keys on `ACK_FAIL` specifically instead of on "not success"
    /// stops recognising a classified refusal the moment a host starts classifying, and falls through to
    /// whatever it does for transport death — which for a share group means destroying it. That failure
    /// would look exactly like the classification working, until a newly classified refusal killed a
    /// context. Values 7 and above are deliberately included: they are not sent today.
    #[test]
    fn every_non_success_acknowledgement_is_a_refusal() {
        for ack in 0u8..=255 {
            if ack == ACK_OK {
                continue;
            }
            // Recognised or not, it classifies as some refusal and never as success.
            let kind = RefusalKind::from_ack(ack);
            assert_ne!(
                kind.ack(),
                ACK_OK,
                "ack {ack} must not round-trip to success"
            );
        }
        // And the classified values this host sends are all distinct, so no two reasons collide.
        let sent = [
            RefusalKind::Unstated,
            RefusalKind::Unsupported,
            RefusalKind::ResourceLimit,
            RefusalKind::Invalid,
            RefusalKind::OutOfBounds,
            RefusalKind::UnknownId,
            RefusalKind::Kernel,
            RefusalKind::MappedElsewhere,
        ];
        // A hand-written list silently stops covering the thing it guards the moment a variant is
        // added — `Kernel` was added and this list did not notice. The match below is enumerated by the
        // compiler, so a new variant fails the BUILD until it is named, and the length check then forces
        // it into `sent` too. Together they are the only way this list stays honest.
        for kind in sent {
            match kind {
                RefusalKind::Unstated
                | RefusalKind::Unsupported
                | RefusalKind::ResourceLimit
                | RefusalKind::Invalid
                | RefusalKind::OutOfBounds
                | RefusalKind::UnknownId
                | RefusalKind::Kernel
                | RefusalKind::MappedElsewhere => {}
            }
        }
        assert_eq!(
            sent.len(),
            8,
            "a refusal class was added; name it in `sent` so its byte is checked for collisions"
        );
        let mut bytes: Vec<u8> = sent.iter().map(|k| k.ack()).collect();
        bytes.sort_unstable();
        let count = bytes.len();
        bytes.dedup();
        assert_eq!(bytes.len(), count, "two refusal classes share a byte");
    }
}
