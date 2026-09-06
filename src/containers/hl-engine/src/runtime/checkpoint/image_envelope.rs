//! Stable discriminator for the checkpoint payload stored in one generation.
//!
//! The envelope is encoded field by field rather than by copying a Rust or C
//! structure, so host endianness, padding and compiler layout never enter the
//! persisted format.

pub(super) const OBJECT: &str = "IMAGE";
pub(super) const SIZE: usize = 64;

const MAGIC: &[u8; 8] = b"HLIMAGE\0";
const ENVELOPE_VERSION: u16 = 1;
const TRANSLATED_KIND: u16 = 1;
const NATIVE_X86_V1_KIND: u16 = 2;
const TRANSLATED_MANIFEST_VERSION: u32 = 8;
const NATIVE_X86_V1_VERSION: u32 = 1;
const MANIFEST: &[u8] = b"MANIFEST";

/// The reader selected by a validated `IMAGE` object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Reader {
    /// The existing translated-engine `MANIFEST` reader.
    Translated,
    /// The version-one native x86 image reader, recognized but not yet wired.
    NativeX86V1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Invalid {
    Size,
    Magic,
    EnvelopeVersion,
    Kind,
    PayloadVersion,
    PayloadName,
    MissingTerminator,
    NonzeroTail,
    NonzeroReserved,
}

impl Reader {
    /// Encodes the canonical 64-byte envelope.
    ///
    /// Byte offsets are fixed: magic `0..8`, little-endian envelope version
    /// `8..10`, little-endian kind `10..12`, little-endian payload version
    /// `12..16`, NUL-terminated payload object name with a zero tail `16..48`,
    /// and zero-reserved bytes `48..64`.
    pub(super) fn encode(self) -> [u8; SIZE] {
        let (kind, payload_version) = match self {
            Self::Translated => (TRANSLATED_KIND, TRANSLATED_MANIFEST_VERSION),
            Self::NativeX86V1 => (NATIVE_X86_V1_KIND, NATIVE_X86_V1_VERSION),
        };
        let mut bytes = [0_u8; SIZE];
        bytes[0..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&ENVELOPE_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&kind.to_le_bytes());
        bytes[12..16].copy_from_slice(&payload_version.to_le_bytes());
        bytes[16..16 + MANIFEST.len()].copy_from_slice(MANIFEST);
        bytes
    }

    /// Decodes only a canonical envelope for a reader this host recognizes.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        let bytes: &[u8; SIZE] = bytes.try_into().map_err(|_| Invalid::Size)?;
        if &bytes[0..8] != MAGIC {
            return Err(Invalid::Magic);
        }
        if u16::from_le_bytes(bytes[8..10].try_into().expect("fixed envelope field")) != ENVELOPE_VERSION {
            return Err(Invalid::EnvelopeVersion);
        }
        let reader = match u16::from_le_bytes(bytes[10..12].try_into().expect("fixed envelope field")) {
            TRANSLATED_KIND => Self::Translated,
            NATIVE_X86_V1_KIND => Self::NativeX86V1,
            _ => return Err(Invalid::Kind),
        };
        let payload_version = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed envelope field"));
        let expected_version = match reader {
            Self::Translated => TRANSLATED_MANIFEST_VERSION,
            Self::NativeX86V1 => NATIVE_X86_V1_VERSION,
        };
        if payload_version != expected_version {
            return Err(Invalid::PayloadVersion);
        }
        let name = &bytes[16..48];
        let terminator = name
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Invalid::MissingTerminator)?;
        if name[terminator + 1..].iter().any(|byte| *byte != 0) {
            return Err(Invalid::NonzeroTail);
        }
        if &name[..terminator] != MANIFEST {
            return Err(Invalid::PayloadName);
        }
        if bytes[48..64].iter().any(|byte| *byte != 0) {
            return Err(Invalid::NonzeroReserved);
        }
        Ok(reader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_envelopes_are_exact_and_round_trip() {
        for reader in [Reader::Translated, Reader::NativeX86V1] {
            let encoded = reader.encode();
            assert_eq!(encoded.len(), SIZE);
            assert_eq!(&encoded[0..8], b"HLIMAGE\0");
            assert_eq!(Reader::decode(&encoded), Ok(reader));
        }
        let translated = Reader::Translated.encode();
        assert_eq!(&translated[8..10], &1_u16.to_le_bytes());
        assert_eq!(&translated[10..12], &1_u16.to_le_bytes());
        assert_eq!(&translated[12..16], &8_u32.to_le_bytes());
        assert_eq!(&translated[16..25], b"MANIFEST\0");
        assert!(translated[25..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn translated_payload_version_tracks_the_current_c_manifest_reader() {
        let capture = include_str!("../../../../../runtime/hl-native/src/native/linux_abi/checkpoint/capture.c");
        assert!(
            capture.lines().any(|line| line.starts_with("#define CKPT_VERSION 8 ")),
            "update the translated IMAGE payload version with CKPT_VERSION"
        );
    }

    #[test]
    fn every_noncanonical_field_is_refused() {
        let canonical = Reader::Translated.encode();
        let mut cases = Vec::new();
        cases.push((canonical[..SIZE - 1].to_vec(), Invalid::Size));
        cases.push(([canonical.as_slice(), &[0]].concat(), Invalid::Size));
        for (at, value, expected) in [
            (0, b'X', Invalid::Magic),
            (8, 2, Invalid::EnvelopeVersion),
            (10, 3, Invalid::Kind),
            (12, 9, Invalid::PayloadVersion),
            (16, b'X', Invalid::PayloadName),
            (25, b'X', Invalid::NonzeroTail),
            (48, 1, Invalid::NonzeroReserved),
        ] {
            let mut bytes = canonical;
            bytes[at] = value;
            cases.push((bytes.to_vec(), expected));
        }
        let mut unterminated = canonical;
        unterminated[16..48].fill(b'X');
        cases.push((unterminated.to_vec(), Invalid::MissingTerminator));

        for (bytes, expected) in cases {
            assert_eq!(Reader::decode(&bytes), Err(expected), "mutation: {bytes:?}");
        }
    }
}
