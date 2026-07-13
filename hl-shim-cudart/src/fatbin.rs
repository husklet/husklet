//! Clean-room nvcc fatbin container walker — a faithful Rust port of `hl-gpu/cuda/fatbin.h`.
//!
//! `__cudaRegisterFatBinary` hands cudart the `__fatBinC_Wrapper_t` nvcc emits; on first launch we walk
//! the wrapped fatbin container and extract the embedded UNCOMPRESSED PTX text, which then goes to the
//! driver's `cuModuleLoadData` (dd's PTX front-end). In the C world the *driver's* `cuModuleLoadData`
//! does this walk; dd's Rust driver (`dd-shim-cuda`) treats its image as raw PTX text, so — exactly like
//! the C oracle's design intent (extraction "lives in ONE place") — cudart performs the walk here and
//! passes the recovered PTX down. No NVIDIA source is used; the layout is the documented/community
//! public format (see docs/ideas/CUDART_PLAN.md §2).
//!
//! Tier-1 scope: UNCOMPRESSED PTX only. A compressed entry (flag 0x2000) or a SASS-only (ELF/CUBIN)
//! fatbin yields `None` → the caller surfaces a clean `cudaErrorInvalidKernelImage` (never a crash,
//! never a fake success). All reads are bounded by the container's self-declared `fat_size`, matching
//! the C walker, so a malformed/truncated container returns `None` rather than reading out of range.

use core::ffi::c_void;

const WRAPPER_MAGIC: u32 = 0x466243b1;
const FATBIN_MAGIC: u32 = 0xba55ed50;
const KIND_PTX: u16 = 1;
const FLAG_COMPRESS: u64 = 0x0000_0000_0000_2000;

const HEADER_SIZE: usize = 16; // sizeof(DdFatBinHeader)
const ENTRY_SIZE: usize = 64; // sizeof(DdFatBinEntry)

#[inline]
unsafe fn rd_u16(p: *const u8, off: usize) -> u16 {
    core::ptr::read_unaligned(p.add(off) as *const u16)
}
#[inline]
unsafe fn rd_u32(p: *const u8, off: usize) -> u32 {
    core::ptr::read_unaligned(p.add(off) as *const u32)
}
#[inline]
unsafe fn rd_u64(p: *const u8, off: usize) -> u64 {
    core::ptr::read_unaligned(p.add(off) as *const u64)
}
#[inline]
unsafe fn rd_ptr(p: *const u8, off: usize) -> *const u8 {
    core::ptr::read_unaligned(p.add(off) as *const usize) as *const u8
}

/// Extract the first uncompressed PTX entry as a `Vec<u8>` of the exact text (trailing NUL padding
/// trimmed; NOT itself NUL-terminated — the caller makes a `CString`). `None` for: not a fatbin, no PTX
/// entry (SASS-only), or PTX present but compressed.
///
/// # Safety
/// `image` must be null or point at a fatbin wrapper / container that is valid for the bytes its own
/// header declares (`header_size + fat_size`), exactly as the C `dd_fatbin_extract_ptx` requires.
pub unsafe fn extract_ptx(image: *const c_void) -> Option<Vec<u8>> {
    if image.is_null() {
        return None;
    }
    let ip = image as *const u8;
    let m0 = rd_u32(ip, 0);
    let container: *const u8 = if m0 == WRAPPER_MAGIC {
        // __fatBinC_Wrapper_t { int magic; int version; const void* data; void* filename; }
        let data = rd_ptr(ip, 8); // `data` field
        if data.is_null() {
            return None;
        }
        data
    } else if m0 == FATBIN_MAGIC {
        ip
    } else {
        return None;
    };

    let magic = rd_u32(container, 0);
    let header_size = rd_u16(container, 6) as usize;
    if magic != FATBIN_MAGIC || header_size < HEADER_SIZE {
        return None;
    }
    let fat_size = rd_u64(container, 8) as usize;
    let base = container.add(header_size);
    let end = base as usize + fat_size;
    let mut cur = base as usize;

    while cur + ENTRY_SIZE <= end {
        let ep = cur as *const u8;
        let entry_hdr = rd_u32(ep, 4) as usize; // header_size
        if entry_hdr < ENTRY_SIZE {
            break;
        }
        if end - cur < entry_hdr {
            break;
        }
        let payload = cur + entry_hdr;
        let payload_size = rd_u64(ep, 8) as usize;
        if end - payload < payload_size {
            break;
        }
        let kind = rd_u16(ep, 0);
        let flags = rd_u64(ep, 40);
        if kind == KIND_PTX && (flags & FLAG_COMPRESS) == 0 {
            let pp = payload as *const u8;
            // PTX payloads are NUL-padded; trim the trailing NUL run so the copy is exact text.
            let mut n = payload_size;
            while n > 0 && *pp.add(n - 1) == 0 {
                n -= 1;
            }
            let mut v = vec![0u8; n];
            core::ptr::copy_nonoverlapping(pp, v.as_mut_ptr(), n);
            return Some(v);
        }
        cur = payload + payload_size;
    }
    None
}
