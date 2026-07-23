use super::*;

// ===================================================================================================
// fatbin walker — malformed / adversarial containers must yield None (never a crash, never fake PTX)
// ===================================================================================================

const FATBIN_MAGIC: u32 = 0xba55_ed50;
const FLAG_COMPRESS: u64 = 0x2000;

/// Build a single-entry fatbin container with a chosen entry `kind` and `flags`.
fn fatbin_with(kind: u16, flags: u64, payload: &[u8]) -> Vec<u8> {
    let mut entry = vec![0u8; 64];
    entry[0..2].copy_from_slice(&kind.to_le_bytes());
    entry[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    entry[8..16].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    entry[40..48].copy_from_slice(&flags.to_le_bytes());
    let fat_size = (entry.len() + payload.len()) as u64;
    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(&FATBIN_MAGIC.to_le_bytes());
    out[6..8].copy_from_slice(&16u16.to_le_bytes()); // container header_size
    out[8..16].copy_from_slice(&fat_size.to_le_bytes());
    out.extend_from_slice(&entry);
    out.extend_from_slice(payload);
    out
}

#[test]
fn fatbin_rejects_compressed_ptx_sass_only_and_truncated() {
    // A COMPRESSED PTX entry is out of the tier-1 scope → None (never a garbled decompress).
    let compressed = fatbin_with(1, FLAG_COMPRESS, b".version 7.5\n");
    assert!(fatbin::Image::new(&compressed).is_fatbin());
    assert_eq!(fatbin::Image::new(&compressed).ptx(), None);

    // A SASS/ELF-only fatbin (kind != PTX) carries no PTX → None.
    let sass = fatbin_with(2, 0, b"\x7fELF-ish");
    assert_eq!(fatbin::Image::new(&sass).ptx(), None);

    // A container whose self-declared fat_size runs past the slice is truncated → None.
    let mut trunc = fatbin_with(1, 0, b".version 7.5\n");
    let bad_size = (trunc.len() as u64) + 4096;
    trunc[8..16].copy_from_slice(&bad_size.to_le_bytes());
    assert_eq!(fatbin::Image::new(&trunc).ptx(), None);
}

#[test]
fn fatbin_trims_trailing_nul_padding_of_the_ptx_payload() {
    // A PTX payload is NUL-padded on disk; the walker must return the exact text without the padding.
    let mut padded = b".version 7.5\n".to_vec();
    padded.extend_from_slice(&[0u8; 8]); // NUL padding
    let img = fatbin_with(1, 0, &padded);
    assert_eq!(fatbin::Image::new(&img).ptx().unwrap(), b".version 7.5\n");
}
