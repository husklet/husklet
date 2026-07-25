//! Present a rendered swapchain frame into the app's OWN `wl_surface` — the surface a real Vulkan
//! wayland app (e.g. `vkcube --wsi wayland`) created on its OWN `libwayland-client` connection at
//! `vkCreateWaylandSurfaceKHR`.
//!
//! This is the Vulkan analog of the GL milestone in `hl-gl/src/adapter/wayland_app.rs`: the frame
//! that [`crate::service::present::read_presented_image`] reads back off the presented swapchain image is
//! marshalled as a `wl_shm` `wl_buffer` and `attach`/`damage`/`commit`ed onto the app's `wl_surface`
//! (captured in `VkWaylandSurfaceCreateInfoKHR`). No socket is opened here — the app already owns the
//! connection; we reach it through the surface proxy and drive it via the app's already-mapped
//! `libwayland-client`.
//!
//! `libwayland-client` owns the app's connection: the send buffer, the object-id space, the fd-passing
//! ring. A second raw socket cannot address the app's `wl_surface` (a different id space). So we must
//! marshal through the app's OWN `libwayland-client` — `dlopen(RTLD_NOLOAD)` the already-loaded copy,
//! `dlsym` the proxy/queue ABI, and `wl_proxy_marshal_flags` our requests (the Mesa EGL-Wayland pattern).
//! The shim MUST NOT reenter the app's own event listeners. So it creates a PRIVATE `wl_event_queue`
//! (`wl_display_create_queue`) and wraps every proxy it creates/uses with `wl_proxy_create_wrapper` +
//! `wl_proxy_set_queue` so their events dispatch to OUR queue only. We never `roundtrip` the app's default
//! queue and never install a competing frame callback on the app's surface — only `attach`+`commit`+`flush`.
//! Every fallible step returns a typed [`WlAppError`]; a missing library / symbol / global yields a *soft*
//! error the caller maps to `VK_SUCCESS` (the readback still happened, only the on-surface attach was
//! skipped — headless/offscreen present) — never a faked-up wayland backend. A live marshal/flush failure
//! is a *hard* error the caller surfaces as `VK_ERROR_OUT_OF_DATE_KHR` / `VK_ERROR_SURFACE_LOST_KHR`.
//! Unlike GL (`glReadPixels` is **bottom-left** origin), a Vulkan swapchain image reads back **top-left**
//! origin — so [`pixels_to_xrgb8888`] does NOT flip vertically. The presented image reads back in its
//! native texel order (`Bgra8`/`Rgba8` per the surface format); the convert reorders both into the
//! `WL_SHM_FORMAT_XRGB8888` little-endian `[B,G,R,X]` byte order a `wl_shm` buffer wants.
//! The `libwayland-client` ABI is behind the [`WlAbi`] trait. The live [`SysWlAbi`] is `dlopen`/`dlsym`;
//! a recording backend (in tests) captures every marshalled request so the opcode/arg layout + the
//! private-queue wrapper wiring + the dlsym-fallback path are unit-testable WITHOUT a live compositor.

mod abi;
mod present;
mod session;
mod shared_memory;

#[cfg(test)]
use crate::result::{VK_ERROR_OUT_OF_DATE_KHR, VK_ERROR_SURFACE_LOST_KHR, VK_SUCCESS};
#[cfg(test)]
use abi::{SysWlAbi, WlAbi};
#[cfg(test)]
use core::ffi::c_void;
#[cfg(test)]
use present::FramePlane;
pub use present::{pixels_to_xrgb8888, WlAppError, WlAppResult};
pub use session::WaylandAppPresenter;
// ==================================================================================================
// tests — a recording backend proves the request opcodes/args + wrapper wiring without a compositor
// ==================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One recorded ABI interaction (the marshalled request or an infra op).
    #[derive(Debug, Clone, PartialEq)]
    enum Rec {
        GetDisplay(usize),
        CreateQueue(usize),
        CreateWrapper(usize),
        WrapperDestroy(usize),
        SetQueue(usize, usize),
        Destroy(usize),
        Flush(usize),
        GetRegistry {
            on: usize,
        },
        DiscoverShm {
            registry: usize,
        },
        BindShm {
            registry: usize,
            name: u32,
            version: u32,
        },
        ShmCreatePool {
            shm: usize,
            fd_valid: bool,
            size: i32,
        },
        PoolCreateBuffer {
            pool: usize,
            w: i32,
            h: i32,
            stride: i32,
            format: u32,
        },
        PoolDestroy(usize),
        BufferDestroy(usize),
        Attach {
            surface: usize,
            buffer: usize,
        },
        Damage {
            surface: usize,
            w: i32,
            h: i32,
        },
        Commit {
            surface: usize,
        },
    }

    /// A recording `WlAbi`: hands out fresh opaque pointer identities and logs every call so the request
    /// opcodes/args + private-queue wrapper wiring are assertable with no live compositor.
    struct Recorder {
        log: RefCell<Vec<Rec>>,
        next: RefCell<usize>,
        /// Whether `discover_shm` should report a wl_shm global (false models a compositor without shm).
        has_shm: bool,
        /// Force a constructor to return null (models a live marshal failure).
        fail_pool: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder {
                log: RefCell::new(Vec::new()),
                next: RefCell::new(0x1000),
                has_shm: true,
                fail_pool: false,
            }
        }
        fn fresh(&self) -> *mut c_void {
            let mut n = self.next.borrow_mut();
            *n += 0x10;
            *n as *mut c_void
        }
        fn push(&self, r: Rec) {
            self.log.borrow_mut().push(r);
        }
        fn log(&self) -> Vec<Rec> {
            self.log.borrow().clone()
        }
    }

    // SAFETY: recorder pointer values are opaque identities; it never dereferences them.
    unsafe impl WlAbi for Recorder {
        fn get_display(&self, surface: *mut c_void) -> *mut c_void {
            self.push(Rec::GetDisplay(surface as usize));
            0xD15_9000usize as *mut c_void // a fixed non-null "app display"
        }
        fn get_version(&self, _proxy: *mut c_void) -> u32 {
            4
        }
        fn create_queue(&self, display: *mut c_void) -> *mut c_void {
            self.push(Rec::CreateQueue(display as usize));
            0x0000_9EE0_usize as *mut c_void // a fixed non-null "private queue"
        }
        fn create_wrapper(&self, proxy: *mut c_void) -> *mut c_void {
            self.push(Rec::CreateWrapper(proxy as usize));
            self.fresh()
        }
        fn wrapper_destroy(&self, wrapper: *mut c_void) {
            self.push(Rec::WrapperDestroy(wrapper as usize));
        }
        fn set_queue(&self, proxy: *mut c_void, queue: *mut c_void) {
            self.push(Rec::SetQueue(proxy as usize, queue as usize));
        }
        fn destroy(&self, proxy: *mut c_void) {
            self.push(Rec::Destroy(proxy as usize));
        }
        fn flush(&self, display: *mut c_void) -> i32 {
            self.push(Rec::Flush(display as usize));
            0
        }
        fn get_registry(&self, display_wrapper: *mut c_void, _version: u32) -> *mut c_void {
            self.push(Rec::GetRegistry {
                on: display_wrapper as usize,
            });
            self.fresh()
        }
        fn discover_shm(
            &self,
            registry: *mut c_void,
            _display: *mut c_void,
            _queue: *mut c_void,
        ) -> Option<(u32, u32)> {
            self.push(Rec::DiscoverShm {
                registry: registry as usize,
            });
            if self.has_shm {
                Some((7, 1))
            } else {
                None
            }
        }
        fn bind_shm(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void {
            self.push(Rec::BindShm {
                registry: registry as usize,
                name,
                version,
            });
            self.fresh()
        }
        fn shm_create_pool(
            &self,
            shm: *mut c_void,
            _version: u32,
            fd: i32,
            size: i32,
        ) -> *mut c_void {
            self.push(Rec::ShmCreatePool {
                shm: shm as usize,
                fd_valid: fd >= 0,
                size,
            });
            if self.fail_pool {
                core::ptr::null_mut()
            } else {
                self.fresh()
            }
        }
        fn pool_create_buffer(
            &self,
            pool: *mut c_void,
            _version: u32,
            w: i32,
            h: i32,
            stride: i32,
            format: u32,
        ) -> *mut c_void {
            self.push(Rec::PoolCreateBuffer {
                pool: pool as usize,
                w,
                h,
                stride,
                format,
            });
            self.fresh()
        }
        fn pool_destroy(&self, pool: *mut c_void, _version: u32) {
            self.push(Rec::PoolDestroy(pool as usize));
        }
        fn buffer_destroy(&self, buffer: *mut c_void, _version: u32) {
            self.push(Rec::BufferDestroy(buffer as usize));
        }
        fn surface_attach(&self, surface: *mut c_void, _version: u32, buffer: *mut c_void) {
            self.push(Rec::Attach {
                surface: surface as usize,
                buffer: buffer as usize,
            });
        }
        fn surface_damage(&self, surface: *mut c_void, _version: u32, w: i32, h: i32) {
            self.push(Rec::Damage {
                surface: surface as usize,
                w,
                h,
            });
        }
        fn surface_commit(&self, surface: *mut c_void, _version: u32) {
            self.push(Rec::Commit {
                surface: surface as usize,
            });
        }
    }

    const SURFACE: *mut c_void = 0xA9900usize as *mut c_void;

    fn xrgb(w: usize, h: usize) -> Vec<u8> {
        // A non-blank plane (opaque white) so `plane_is_present` passes.
        vec![0xFFu8; w * h * 4]
    }

    /// Bring-up derives the display FROM the surface proxy (no socket), creates a private queue, and binds
    /// wl_shm off the app registry with the DISCOVERED name — the whole isolation contract in one trace.
    #[test]
    fn bringup_derives_display_and_binds_shm_on_private_queue() {
        let rec = Box::new(Recorder::new());
        let p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        assert_eq!(p.shm_version, 1);
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();

        // 1) The display is derived from the app's surface proxy — proving no own socket is opened.
        assert_eq!(log[0], Rec::GetDisplay(SURFACE as usize));
        // 2) A private queue is created off that app display.
        assert_eq!(log[1], Rec::CreateQueue(0xD15_9000));
        // 3) A display wrapper is created + pinned to the private queue, and get_registry runs on it.
        assert!(matches!(log[2], Rec::CreateWrapper(0xD15_9000)));
        assert!(matches!(log[3], Rec::SetQueue(_, 0x0000_9EE0)));
        assert!(matches!(log[4], Rec::GetRegistry { .. }));
        // 4) wl_shm is discovered then bound with the DISCOVERED registry name (7) at the discovered version.
        assert!(log.iter().any(|r| matches!(r, Rec::DiscoverShm { .. })));
        assert!(log.iter().any(|r| matches!(
            r,
            Rec::BindShm {
                name: 7,
                version: 1,
                ..
            }
        )));
        // 5) The app surface is wrapped + pinned to the private queue (never disturbing the app's queue).
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::CreateWrapper(a) if *a == SURFACE as usize)));
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::SetQueue(_, 0x0000_9EE0))));
    }

    /// A present marshals pool → buffer → attach → damage → commit → flush with the right args, onto the
    /// surface WRAPPER (private queue), and passes a valid shm fd.
    #[test]
    fn present_marshals_pool_buffer_attach_damage_commit_flush() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        let surface_wrapper = p.surface_wrapper as usize;
        // Clear the bring-up trace to focus on the frame.
        unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }
            .log
            .borrow_mut()
            .clear();

        p.present(&xrgb(4, 3), 4, 3).expect("present");
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();

        // create_pool with a real fd + full byte size (4*3*4 = 48).
        let pool_rec = log.iter().find_map(|r| match r {
            Rec::ShmCreatePool {
                shm: _,
                fd_valid,
                size,
            } => Some((*fd_valid, *size)),
            _ => None,
        });
        assert_eq!(pool_rec, Some((true, 48)));
        // create_buffer at 4x3, stride 16, XRGB8888.
        assert!(log.iter().any(|r| matches!(
            r,
            Rec::PoolCreateBuffer {
                w: 4,
                h: 3,
                stride: 16,
                format: 1,
                ..
            }
        )));
        // The pool is destroyed once the buffer holds the mapping.
        assert!(log.iter().any(|r| matches!(r, Rec::PoolDestroy(_))));
        // attach/damage/commit all target the SURFACE WRAPPER (the private-queue proxy), not the raw surface.
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::Attach { surface, .. } if *surface == surface_wrapper)));
        assert!(log.iter().any(
            |r| matches!(r, Rec::Damage { surface, w: 4, h: 3 } if *surface == surface_wrapper)
        ));
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::Commit { surface } if *surface == surface_wrapper)));
        assert_ne!(
            surface_wrapper, SURFACE as usize,
            "commit must go via a wrapper, not the app's raw surface"
        );
        // A flush ends the frame.
        assert!(matches!(log.last(), Some(Rec::Flush(_))));
    }

    /// Frame N+1 retires frame N's buffer before superseding it (double-buffer safety, no unbounded leak).
    #[test]
    fn second_frame_retires_the_previous_buffer() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        p.present(&xrgb(2, 2), 2, 2).expect("frame 1");
        unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }
            .log
            .borrow_mut()
            .clear();
        p.present(&xrgb(2, 2), 2, 2).expect("frame 2");
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();
        // The very first op of frame 2 destroys frame 1's buffer.
        assert!(
            matches!(log.first(), Some(Rec::BufferDestroy(_))),
            "frame 2 must retire the prior buffer first"
        );
    }

    /// A compositor without wl_shm fails bring-up LOUDLY (soft error → readback-only present), never fakes it.
    #[test]
    fn missing_shm_global_is_a_soft_error() {
        let mut rec = Recorder::new();
        rec.has_shm = false;
        let err = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE)
            .err()
            .unwrap();
        assert_eq!(err, WlAppError::NoShmGlobal);
        assert!(
            err.is_unavailable(),
            "a missing global must be a soft (readback-only) failure"
        );
        assert_eq!(err.to_vk_result(), VK_SUCCESS);
    }

    /// A null surface pointer (not a wayland app) is a soft NoSurface — the caller keeps its readback path.
    #[test]
    fn null_surface_is_soft_no_surface() {
        let rec = Box::new(Recorder::new());
        let err = WaylandAppPresenter::with_abi(rec, core::ptr::null_mut())
            .err()
            .unwrap();
        assert_eq!(err, WlAppError::NoSurface);
        assert!(err.is_unavailable());
        assert_eq!(err.to_vk_result(), VK_SUCCESS);
    }

    /// A too-small readback plane is a hard BadSize → VK_ERROR_OUT_OF_DATE_KHR (never a silent/blank present).
    #[test]
    fn short_plane_is_hard_bad_size() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        let err = p.present(&[0u8; 4], 4, 4).unwrap_err();
        assert_eq!(err, WlAppError::BadSize);
        assert!(
            !err.is_unavailable(),
            "a live present failure is hard, not readback-only"
        );
        assert_eq!(err.to_vk_result(), VK_ERROR_OUT_OF_DATE_KHR);
    }

    /// A constructor returning null (a live marshal failure) is a hard Marshal → VK_ERROR_OUT_OF_DATE_KHR.
    #[test]
    fn null_constructor_is_hard_marshal_error() {
        let mut rec = Recorder::new();
        rec.fail_pool = true;
        let mut p = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE).expect("bring-up");
        let err = p.present(&xrgb(2, 2), 2, 2).unwrap_err();
        assert_eq!(err, WlAppError::Marshal);
        assert!(!err.is_unavailable());
        assert_eq!(err.to_vk_result(), VK_ERROR_OUT_OF_DATE_KHR);
    }

    /// The soft/hard classification + VkResult mapping is the exact contract the caller keys on.
    #[test]
    fn error_softness_and_vk_result_mapping() {
        for e in [
            WlAppError::NoSurface,
            WlAppError::LibraryMissing,
            WlAppError::SymbolMissing("wl_proxy_marshal_flags"),
            WlAppError::NoDisplay,
            WlAppError::QueueSetup,
            WlAppError::NoShmGlobal,
        ] {
            assert!(e.is_unavailable(), "{e:?} must be soft");
            assert_eq!(e.to_vk_result(), VK_SUCCESS, "{e:?} soft → VK_SUCCESS");
        }
        // Hard, connection/allocation loss → SURFACE_LOST.
        for e in [WlAppError::ShmAlloc, WlAppError::Flush] {
            assert!(!e.is_unavailable(), "{e:?} must be hard");
            assert_eq!(e.to_vk_result(), VK_ERROR_SURFACE_LOST_KHR);
        }
        // Hard, per-frame marshal/size → OUT_OF_DATE.
        for e in [WlAppError::BadSize, WlAppError::Marshal] {
            assert!(!e.is_unavailable(), "{e:?} must be hard");
            assert_eq!(e.to_vk_result(), VK_ERROR_OUT_OF_DATE_KHR);
        }
    }

    /// The live `dlopen(RTLD_NOLOAD)` load path: with `libwayland-client` NOT mapped into this test
    /// process, `SysWlAbi::load()` returns a typed soft error (never a null-fn backend, never a fake up).
    #[test]
    fn sys_abi_load_without_libwayland_is_a_soft_error() {
        // The test harness does not link/load libwayland-client, so RTLD_NOLOAD must miss.
        match SysWlAbi::load() {
            Err(e) => assert!(
                e.is_unavailable(),
                "absent libwayland must be a soft error, got {e:?}"
            ),
            Ok(_) => { /* if a host happens to have it mapped, the load simply succeeded — also valid. */
            }
        }
    }

    /// The Bgra8 readback (native XRGB order) passes through unswapped; alpha is forced opaque.
    #[test]
    fn convert_bgra_passthrough_top_left() {
        // one texel: B=0x11 G=0x22 R=0x33 A=0x44
        let src = vec![0x11, 0x22, 0x33, 0x44];
        let out = pixels_to_xrgb8888(&src, 1, 1, true);
        assert_eq!(out, vec![0x11, 0x22, 0x33, 0xFF]); // [B,G,R,X]
    }

    /// The Rgba8 readback swaps R↔B into XRGB order; no vertical flip (Vulkan is top-left, unlike GL).
    #[test]
    fn convert_rgba_swaps_channels_no_flip() {
        // row 0 texel: R=0xAA G=0xBB B=0xCC ; row 1 texel: R=0x10 G=0x20 B=0x30
        let src = vec![0xAA, 0xBB, 0xCC, 0xFF, 0x10, 0x20, 0x30, 0xFF];
        let out = pixels_to_xrgb8888(&src, 1, 2, false);
        // top-left origin preserved: row 0 first, packed [B,G,R,X].
        assert_eq!(&out[0..4], &[0xCC, 0xBB, 0xAA, 0xFF]);
        assert_eq!(&out[4..8], &[0x30, 0x20, 0x10, 0xFF]);
    }

    /// A short source returns the all-zero fill (which `plane_is_present` then rejects as a failed readback).
    #[test]
    fn convert_short_source_is_all_zero() {
        let out = pixels_to_xrgb8888(&[0u8; 3], 2, 2, true);
        assert_eq!(out, vec![0u8; 16]);
        assert!(!FramePlane::is_present(&out));
    }
}
