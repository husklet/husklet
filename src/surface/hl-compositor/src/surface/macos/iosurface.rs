//! IOSurface FFI (macOS). objc2 binds no IOSurface, so declare the C API + link the framework — the same
//! declarations the old `hl-display::metal` used. An `IOSurface` is the host GPU allocation a guest's
//! `linux-dmabuf`/zero-copy buffer is backed by: the compositor wraps it as an `MTLTexture` with no copy
//! (see [`super::metal::MetalCtx::texture_from_iosurface`]).

use objc2::rc::Retained;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use std::ffi::c_void;
use std::ptr::NonNull;

/// Raw IOSurface handle used only at the native Metal boundary.
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
    fn IOSurfaceGetBytesPerRow(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetWidth(s: IOSurfaceRef) -> usize;
    fn IOSurfaceGetHeight(s: IOSurfaceRef) -> usize;
    /// Resolve a global IOSurface id (the engine's alloc id) to a surface. Restricted on modern macOS —
    /// the real path caches send-rights over a mach bridge; this is the standalone-crate fallback.
    fn IOSurfaceLookup(id: u32) -> IOSurfaceRef;
    fn CFRelease(cf: *const c_void);
}

const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241; // 'BGRA'

/// An owned Core Foundation IOSurface reference.
pub struct IOSurface(NonNull<c_void>);

impl IOSurface {
    /// Resolve a global IOSurface id. `IOSurfaceLookup` returns an owned (+1) reference.
    pub fn lookup(id: u32) -> Option<Self> {
        // SAFETY: the C function accepts every u32 id and returns either null or a valid +1 reference.
        NonNull::new(unsafe { IOSurfaceLookup(id) }).map(Self)
    }

    /// Allocate BGRA8888 storage with tightly specified rows.
    pub fn new(w: u32, h: u32) -> Option<Self> {
        unsafe {
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
            NonNull::new(IOSurfaceCreate(Retained::as_ptr(&props) as *const c_void)).map(Self)
        }
    }

    pub fn dimensions(&self) -> (usize, usize, usize) {
        unsafe {
            (
                IOSurfaceGetWidth(self.as_ptr()),
                IOSurfaceGetHeight(self.as_ptr()),
                IOSurfaceGetBytesPerRow(self.as_ptr()),
            )
        }
    }

    pub(crate) fn as_ptr(&self) -> IOSurfaceRef {
        self.0.as_ptr()
    }
}

impl Drop for IOSurface {
    fn drop(&mut self) {
        // SAFETY: constructors accept only +1 references, this type is not Clone, and Drop runs once.
        unsafe { CFRelease(self.0.as_ptr().cast_const()) };
    }
}
