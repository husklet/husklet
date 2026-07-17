//! IOSurface FFI (macOS). objc2 binds no IOSurface, so declare the C API + link the framework — the same
//! declarations the old `hl-display::metal` used. An `IOSurface` is the host GPU allocation a guest's
//! `linux-dmabuf`/zero-copy buffer is backed by: the compositor wraps it as an `MTLTexture` with no copy
//! (see [`super::metal::MetalCtx::texture_from_iosurface`]).

use objc2::rc::Retained;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use std::ffi::c_void;

/// An opaque `IOSurfaceRef` (a CoreFoundation type). Owned refs must be balanced with [`cfrelease`].
pub type IOSurfaceRef = *mut c_void;
type CFStringRef = *const c_void;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    static kIOSurfaceWidth: CFStringRef;
    static kIOSurfaceHeight: CFStringRef;
    static kIOSurfaceBytesPerElement: CFStringRef;
    static kIOSurfaceBytesPerRow: CFStringRef;
    static kIOSurfacePixelFormat: CFStringRef;
    fn IOSurfaceCreate(properties: *const c_void) -> IOSurfaceRef;
    fn IOSurfaceLock(s: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(s: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetBaseAddress(s: IOSurfaceRef) -> *mut c_void;
    fn IOSurfaceGetBytesPerRow(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetWidth(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetHeight(s: IOSurfaceRef) -> usize;
    /// Resolve a global IOSurface id (the engine's alloc id) to a surface. Restricted on modern macOS —
    /// the real path caches send-rights over a mach bridge; this is the standalone-crate fallback.
    fn IOSurfaceLookup(id: u32) -> IOSurfaceRef;
    fn CFRelease(cf: *const c_void);
}

const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241; // 'BGRA'

/// Release a CF/IOSurface reference.
pub unsafe fn cfrelease(s: IOSurfaceRef) {
    if !s.is_null() {
        CFRelease(s as *const c_void);
    }
}

/// Resolve a global IOSurface id to a surface (`IOSurfaceLookup`). `null` if the id is unknown. The
/// caller owns the returned reference (release with [`cfrelease`]).
pub unsafe fn lookup(id: u32) -> IOSurfaceRef {
    IOSurfaceLookup(id)
}

/// `(width, height, bytes_per_row)` of a live IOSurface.
pub unsafe fn dimensions(s: IOSurfaceRef) -> (usize, usize, usize) {
    (
        IOSurfaceGetWidth(s),
        IOSurfaceGetHeight(s),
        IOSurfaceGetBytesPerRow(s),
    )
}

/// Allocate a host `IOSurface` (BGRA8888, `w`×`h`). The host wraps it as an `MTLTexture` with zero copy;
/// this is the buffer a guest's zero-copy allocation would be backed by. Caller owns it ([`cfrelease`]).
/// # Safety
///
/// The returned Core Foundation object has a +1 retain count. The caller must eventually release it
/// with [`cfrelease`] and must not use it after that release.
pub unsafe fn create_iosurface(w: u32, h: u32) -> IOSurfaceRef {
    let k = |s: CFStringRef| &*(s as *const NSString);
    let keys: [&NSString; 5] = [
        k(kIOSurfaceWidth),
        k(kIOSurfaceHeight),
        k(kIOSurfaceBytesPerElement),
        k(kIOSurfaceBytesPerRow),
        k(kIOSurfacePixelFormat),
    ];
    let vals = [
        NSNumber::numberWithInt(w as i32),
        NSNumber::numberWithInt(h as i32),
        NSNumber::numberWithInt(4),
        NSNumber::numberWithInt((w * 4) as i32),
        NSNumber::numberWithInt(PIXEL_FORMAT_BGRA),
    ];
    let props = NSDictionary::from_id_slice(&keys, &vals);
    IOSurfaceCreate(Retained::as_ptr(&props) as *const c_void)
}

/// CPU-fill a freshly created `IOSurface`'s pages with tight BGRA rows (`w*4` bytes per row), honoring the
/// surface's real `bytesPerRow` stride. Plants a known pattern in the surface's storage before wrapping it
/// as a texture (the zero-copy IOSurface present path). Rows beyond `bgra`'s length are left untouched.
#[allow(dead_code)] // forward seam: IOSurface zero-copy fill, exercised once a live IOSurface id is bridged
pub unsafe fn fill_bgra(s: IOSurfaceRef, bgra: &[u8], w: u32, h: u32) {
    IOSurfaceLock(s, 0, std::ptr::null_mut());
    let base = IOSurfaceGetBaseAddress(s) as *mut u8;
    let stride = IOSurfaceGetBytesPerRow(s);
    let tight = (w * 4) as usize;
    for y in 0..h as usize {
        let src = y * tight;
        if src + tight > bgra.len() {
            break;
        }
        std::ptr::copy_nonoverlapping(bgra.as_ptr().add(src), base.add(y * stride), tight);
    }
    IOSurfaceUnlock(s, 0, std::ptr::null_mut());
}
