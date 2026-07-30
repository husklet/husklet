//! Owned macOS IOSurface allocations.

#[cfg(target_os = "macos")]
mod surface {
    use std::ffi::c_void;
    use std::fmt;
    use std::marker::PhantomData;
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    type IOSurfaceRef = *mut c_void;
    type CFStringRef = *const c_void;

    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        static kIOSurfaceWidth: CFStringRef;
        static kIOSurfaceHeight: CFStringRef;
        static kIOSurfaceBytesPerElement: CFStringRef;
        static kIOSurfaceBytesPerRow: CFStringRef;
        static kIOSurfacePixelFormat: CFStringRef;

        fn IOSurfaceCreate(properties: *const c_void) -> IOSurfaceRef;
        fn IOSurfaceGetID(surface: IOSurfaceRef) -> u32;
        fn IOSurfaceGetWidth(surface: IOSurfaceRef) -> usize;
        fn IOSurfaceGetHeight(surface: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBytesPerRow(surface: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBaseAddress(surface: IOSurfaceRef) -> *mut c_void;
        fn IOSurfaceLock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;

        fn CFRetain(value: *const c_void) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }

    const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241;
    const READ_ONLY: u32 = 1;
    const READ_WRITE: u32 = 0;

    /// Failure to allocate or access IOSurface storage.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Error {
        InvalidDimensions,
        Allocation,
        Lock(i32),
        Unlock(i32),
        MissingStorage,
        PixelLength,
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::InvalidDimensions => f.write_str("invalid IOSurface dimensions"),
                Self::Allocation => f.write_str("IOSurface allocation failed"),
                Self::Lock(status) => write!(f, "IOSurface lock failed with status {status}"),
                Self::Unlock(status) => write!(f, "IOSurface unlock failed with status {status}"),
                Self::MissingStorage => f.write_str("IOSurface has no CPU-accessible storage"),
                Self::PixelLength => {
                    f.write_str("BGRA pixel length does not match IOSurface dimensions")
                }
            }
        }
    }

    impl std::error::Error for Error {}

    /// One owned reference to a BGRA IOSurface.
    pub struct Surface(NonNull<c_void>);

    // SAFETY: IOSurface is an OS-managed reference-counted allocation designed for cross-thread sharing.
    // Pixel access uses IOSurfaceLock; GPU synchronization remains the caller's presentation contract.
    unsafe impl Send for Surface {}
    // SAFETY: immutable queries are thread-safe and CPU mutation is guarded by IOSurfaceLock.
    unsafe impl Sync for Surface {}

    impl Surface {
        pub fn new_bgra(width: u32, height: u32) -> Result<Self, Error> {
            let row_bytes = width
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(15))
                .map(|bytes| bytes & !15)
                .filter(|_| width != 0 && height != 0)
                .and_then(|bytes| i32::try_from(bytes).ok())
                .ok_or(Error::InvalidDimensions)?;
            let width = i32::try_from(width).map_err(|_| Error::InvalidDimensions)?;
            let height = i32::try_from(height).map_err(|_| Error::InvalidDimensions)?;

            // SAFETY: framework constants are immortal NSString instances. The dictionary remains alive
            // for IOSurfaceCreate, which copies its property values and returns a +1 reference.
            unsafe {
                let key = |value: CFStringRef| &*(value.cast::<NSString>());
                let keys: [&NSString; 5] = [
                    key(kIOSurfaceWidth),
                    key(kIOSurfaceHeight),
                    key(kIOSurfaceBytesPerElement),
                    key(kIOSurfaceBytesPerRow),
                    key(kIOSurfacePixelFormat),
                ];
                let values = [
                    NSNumber::numberWithInt(width),
                    NSNumber::numberWithInt(height),
                    NSNumber::numberWithInt(4),
                    NSNumber::numberWithInt(row_bytes),
                    NSNumber::numberWithInt(PIXEL_FORMAT_BGRA),
                ];
                let properties: Retained<NSDictionary<NSString, NSNumber>> =
                    NSDictionary::from_id_slice(&keys, &values);
                NonNull::new(IOSurfaceCreate(
                    Retained::as_ptr(&properties).cast::<c_void>(),
                ))
                .map(Self)
                .ok_or(Error::Allocation)
            }
        }

        /// Diagnostic identity only. It is not a transferable capability.
        pub fn id(&self) -> u32 {
            // SAFETY: self owns a live IOSurface reference.
            unsafe { IOSurfaceGetID(self.0.as_ptr()) }
        }

        /// Width, height, and row stride in bytes.
        pub fn dimensions(&self) -> (usize, usize, usize) {
            // SAFETY: self owns a live IOSurface reference.
            unsafe {
                (
                    IOSurfaceGetWidth(self.0.as_ptr()),
                    IOSurfaceGetHeight(self.0.as_ptr()),
                    IOSurfaceGetBytesPerRow(self.0.as_ptr()),
                )
            }
        }

        /// Borrow the opaque native handle for an immediate platform API call.
        pub fn handle(&self) -> Handle<'_> {
            Handle {
                raw: self.0,
                _surface: PhantomData,
            }
        }

        pub fn read_bgra(&self) -> Result<Vec<u8>, Error> {
            let (width, height, stride) = self.dimensions();
            let row = width.checked_mul(4).ok_or(Error::InvalidDimensions)?;
            let len = row.checked_mul(height).ok_or(Error::InvalidDimensions)?;
            self.with_storage(READ_ONLY, |base| {
                let mut pixels = vec![0; len];
                for y in 0..height {
                    // SAFETY: IOSurface reports at least `stride` bytes per row and is locked for reading.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            base.add(y * stride),
                            pixels.as_mut_ptr().add(y * row),
                            row,
                        );
                    }
                }
                Ok(pixels)
            })
        }

        pub fn write_bgra(&self, pixels: &[u8]) -> Result<(), Error> {
            let (width, height, stride) = self.dimensions();
            let row = width.checked_mul(4).ok_or(Error::InvalidDimensions)?;
            if pixels.len() != row.checked_mul(height).ok_or(Error::InvalidDimensions)? {
                return Err(Error::PixelLength);
            }
            self.with_storage(READ_WRITE, |base| {
                for y in 0..height {
                    // SAFETY: IOSurface reports at least `stride` bytes per row and is locked for writing.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            pixels.as_ptr().add(y * row),
                            base.add(y * stride),
                            row,
                        );
                    }
                }
                Ok(())
            })
        }

        fn with_storage<T>(
            &self,
            options: u32,
            use_storage: impl FnOnce(*mut u8) -> Result<T, Error>,
        ) -> Result<T, Error> {
            // SAFETY: self owns a live IOSurface; lock/unlock bracket all CPU access.
            unsafe {
                let status = IOSurfaceLock(self.0.as_ptr(), options, std::ptr::null_mut());
                if status != 0 {
                    return Err(Error::Lock(status));
                }
                let base = IOSurfaceGetBaseAddress(self.0.as_ptr()).cast::<u8>();
                let result = NonNull::new(base)
                    .ok_or(Error::MissingStorage)
                    .and_then(|base| use_storage(base.as_ptr()));
                let status = IOSurfaceUnlock(self.0.as_ptr(), options, std::ptr::null_mut());
                if status != 0 {
                    return Err(Error::Unlock(status));
                }
                result
            }
        }
    }

    impl Clone for Surface {
        fn clone(&self) -> Self {
            // SAFETY: self owns a live CF object; CFRetain creates another owned reference.
            let retained = unsafe { CFRetain(self.0.as_ptr().cast_const()) }.cast_mut();
            Self(NonNull::new(retained).expect("CFRetain preserves non-null IOSurface"))
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            // SAFETY: every Surface instance owns exactly one +1 reference.
            unsafe { CFRelease(self.0.as_ptr().cast_const()) };
        }
    }

    /// A borrowed opaque IOSurface reference.
    #[derive(Clone, Copy)]
    pub struct Handle<'a> {
        raw: NonNull<c_void>,
        _surface: PhantomData<&'a Surface>,
    }

    impl Handle<'_> {
        pub fn as_ptr(self) -> *mut c_void {
            self.raw.as_ptr()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn allocation_reports_dimensions_and_aligned_stride() {
            let surface = Surface::new_bgra(7, 5).expect("allocate");
            let (width, height, stride) = surface.dimensions();
            assert_eq!((width, height), (7, 5));
            assert!(stride >= width * 4);
            assert_eq!(stride % 16, 0);
            assert_ne!(surface.id(), 0);
        }

        #[test]
        fn clone_retains_storage_after_source_drops() {
            let surface = Surface::new_bgra(2, 2).expect("allocate");
            surface
                .write_bgra(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16])
                .expect("write");
            let retained = surface.clone();
            drop(surface);
            assert_eq!(
                retained.read_bgra().expect("read retained allocation"),
                [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub use surface::{Error, Handle, Surface};
