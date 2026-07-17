//! Clean-room nvcc **fatbin** container walker — a byte-slice port of `hl-shim-cudart/src/fatbin.rs`
//! (itself a faithful port of `hl-gpu/cuda/fatbin.h`).
//!
//! `__cudaRegisterFatBinary` hands cudart the `__fatBinC_Wrapper_t` nvcc emits; on `cuModuleLoadData`
//! the driver walks the wrapped container and extracts the embedded UNCOMPRESSED PTX text, which then
//! feeds hl's PTX front-end ([`super::ptx`]). This port takes a `&[u8]` (the image the shim will hand
//! down once the cdylibs exist) instead of a raw `*const c_void`, so it is memory-safe and directly
//! testable without FFI; the field layout + bounds discipline match the C oracle exactly.
//!
//! Tier-1 scope: UNCOMPRESSED PTX only. A compressed entry (flag `0x2000`) or a SASS-only (ELF/CUBIN)
//! fatbin yields `None` → the caller surfaces a clean `cudaErrorInvalidKernelImage` (never a crash,
//! never a fake success). All reads are bounded by the container's self-declared `fat_size`, matching
//! the C walker, so a malformed/truncated container returns `None` rather than reading out of range.

const WRAPPER_MAGIC: u32 = 0x4662_43b1;
const FATBIN_MAGIC: u32 = 0xba55_ed50;
const KIND_PTX: u16 = 1;
const FLAG_COMPRESS: u64 = 0x0000_0000_0000_2000;

const HEADER_SIZE: usize = 16; // sizeof(HlFatBinHeader)
const ENTRY_SIZE: usize = 64; // sizeof(HlFatBinEntry)

fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    b.get(off..off + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// Does `image` begin with a fatbin wrapper or a bare fatbin container magic? Used by
/// `cuModuleLoadData` to decide fatbin-walk vs. raw-PTX-text before it commits to a path.
pub fn is_fatbin(image: &[u8]) -> bool {
    matches!(rd_u32(image, 0), Some(WRAPPER_MAGIC) | Some(FATBIN_MAGIC))
}

/// Extract the first uncompressed PTX entry as exact text bytes (trailing NUL padding trimmed; NOT
/// itself NUL-terminated). `None` for: not a fatbin, no PTX entry (SASS-only), a compressed PTX entry,
/// or a malformed/truncated container.
///
/// Unlike the raw-pointer C oracle, this cannot chase a `data` pointer out of a wrapper into a separate
/// allocation: a wrapper whose container is not embedded in the same slice yields `None` (the shim will
/// hand down the container bytes directly once wired).
pub fn extract_ptx(image: &[u8]) -> Option<Vec<u8>> {
    let m0 = rd_u32(image, 0)?;
    // A wrapper's container lives behind a `const void* data` pointer we cannot follow from a byte
    // slice; only a directly-embedded fatbin container is walkable here.
    let container: &[u8] = if m0 == FATBIN_MAGIC {
        image
    } else if m0 == WRAPPER_MAGIC {
        return None;
    } else {
        return None;
    };

    let magic = rd_u32(container, 0)?;
    let header_size = rd_u16(container, 6)? as usize;
    if magic != FATBIN_MAGIC || header_size < HEADER_SIZE {
        return None;
    }
    let fat_size = rd_u64(container, 8)? as usize;
    let base = header_size;
    let end = base.checked_add(fat_size)?;
    if end > container.len() {
        return None;
    }
    let mut cur = base;

    while cur + ENTRY_SIZE <= end {
        let entry = &container[cur..end];
        let entry_hdr = rd_u32(entry, 4)? as usize; // header_size
        if entry_hdr < ENTRY_SIZE || end - cur < entry_hdr {
            break;
        }
        let payload = cur + entry_hdr;
        let payload_size = rd_u64(entry, 8)? as usize;
        if end.checked_sub(payload)? < payload_size {
            break;
        }
        let kind = rd_u16(entry, 0)?;
        let flags = rd_u64(entry, 40)?;
        if kind == KIND_PTX && (flags & FLAG_COMPRESS) == 0 {
            let pp = &container[payload..payload + payload_size];
            // PTX payloads are NUL-padded; trim the trailing NUL run so the copy is exact text.
            let mut n = payload_size;
            while n > 0 && pp[n - 1] == 0 {
                n -= 1;
            }
            return Some(pp[..n].to_vec());
        }
        cur = payload + payload_size;
    }
    None
}
