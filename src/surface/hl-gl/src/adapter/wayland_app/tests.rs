use super::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
enum Rec {
    GpuSubmit {
        native_present: bool,
    },
    GpuRead,
    GetDisplay(usize),
    CreateQueue(usize),
    CreateWrapper(usize),
    WrapperDestroy(usize),
    SetQueue(usize, usize),
    Destroy(usize),
    DestroyQueue(usize),
    Flush(usize),
    GetRegistry {
        on: usize,
    },
    DiscoverGlobals {
        registry: usize,
    },
    BindShm {
        registry: usize,
        name: u32,
        version: u32,
    },
    BindIdentity {
        name: u32,
        version: u32,
    },
    GetIdentity {
        surface: usize,
    },
    IdentityToken,
    Associate(u64),
    DestroyIdentity,
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
    log: Rc<RefCell<Vec<Rec>>>,
    next: RefCell<usize>,
    /// Whether `discover_shm` should report a wl_shm global (false models a compositor without shm).
    has_shm: bool,
    has_identity: bool,
    identity_token: u64,
    /// Force a constructor to return null (models a live marshal failure).
    fail_pool: bool,
    /// Model a pre-1.23 `libwayland-client`: `wl_proxy_get_display` is absent, so it yields null.
    no_get_display: bool,
}

impl Recorder {
    fn new() -> Self {
        Recorder {
            log: Rc::new(RefCell::new(Vec::new())),
            next: RefCell::new(0x1000),
            has_shm: true,
            has_identity: true,
            identity_token: 0x1234_5678_9abc_def0,
            fail_pool: false,
            no_get_display: false,
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
        if self.no_get_display {
            return core::ptr::null_mut();
        }
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
    fn destroy_queue(&self, queue: *mut c_void) {
        self.push(Rec::DestroyQueue(queue as usize));
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
    fn discover_globals(
        &self,
        registry: *mut c_void,
        _display: *mut c_void,
        _queue: *mut c_void,
    ) -> WlAppResult<Globals> {
        self.push(Rec::DiscoverGlobals {
            registry: registry as usize,
        });
        Ok(Globals {
            shm: self.has_shm.then_some((7, 1)),
            identity: self.has_identity.then_some((8, 1)),
        })
    }
    fn bind_shm(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void {
        self.push(Rec::BindShm {
            registry: registry as usize,
            name,
            version,
        });
        self.fresh()
    }
    fn bind_identity_manager(
        &self,
        _registry: *mut c_void,
        name: u32,
        version: u32,
    ) -> *mut c_void {
        self.push(Rec::BindIdentity { name, version });
        self.fresh()
    }
    fn identity_for_surface(
        &self,
        _manager: *mut c_void,
        _version: u32,
        surface: *mut c_void,
    ) -> *mut c_void {
        self.push(Rec::GetIdentity {
            surface: surface as usize,
        });
        self.fresh()
    }
    fn identity_token(
        &self,
        _identity: *mut c_void,
        _display: *mut c_void,
        _queue: *mut c_void,
    ) -> WlAppResult<SurfaceToken> {
        self.push(Rec::IdentityToken);
        SurfaceToken::new(self.identity_token).map_err(|_| WlAppError::NoIdentity)
    }
    fn identity_associate(&self, _identity: *mut c_void, _version: u32, serial: u64) {
        self.push(Rec::Associate(serial));
    }
    fn identity_destroy(&self, _identity: *mut c_void, _version: u32) {
        self.push(Rec::DestroyIdentity);
    }
    fn shm_create_pool(&self, shm: *mut c_void, _version: u32, fd: i32, size: i32) -> *mut c_void {
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
    // A non-blank plane (opaque white) so `rgba_is_present` passes.
    vec![0xFFu8; w * h * 4]
}

/// Bring-up derives the display FROM the surface proxy (no socket), creates a private queue, and binds
/// wl_shm off the app registry with the DISCOVERED name — the whole isolation contract in one trace.
#[test]
fn bringup_derives_display_and_binds_shm_on_private_queue() {
    let rec = Box::new(Recorder::new());
    let p = WaylandAppPresenter::with_abi(rec, SURFACE, core::ptr::null_mut()).expect("bring-up");
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
    assert!(log.iter().any(|r| matches!(r, Rec::DiscoverGlobals { .. })));
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
    let mut p = WaylandAppPresenter::with_abi(rec, SURFACE, core::ptr::null_mut()).expect("bring-up");
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
    assert!(log
        .iter()
        .any(|r| matches!(r, Rec::Damage { surface, w: 4, h: 3 } if *surface == surface_wrapper)));
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
    let mut p = WaylandAppPresenter::with_abi(rec, SURFACE, core::ptr::null_mut()).expect("bring-up");
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

/// A compositor without wl_shm fails bring-up LOUDLY (soft error → caller falls back), never fakes it.
#[test]
fn missing_shm_global_is_a_soft_error() {
    let mut rec = Recorder::new();
    rec.has_shm = false;
    rec.has_identity = false;
    let err = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE, core::ptr::null_mut())
        .err()
        .unwrap();
    assert_eq!(err, WlAppError::NoShmGlobal);
    assert!(
        err.is_unavailable(),
        "a missing global must be a soft (fall-back) failure"
    );
}

/// A supplied `wl_display*` (the `native_display` the app handed `eglGetDisplay`) is used AS IS:
/// bring-up must never call `wl_proxy_get_display` — that symbol only exists on Wayland 1.23+, and
/// 24.04-era guests ship 1.22. Without this, the presenter never comes up there and every frame falls
/// back to a readback present onto the shim's own mirror window.
#[test]
fn supplied_display_is_used_without_wl_proxy_get_display() {
    const APP_DISPLAY: *mut c_void = 0xDEAD_D150usize as *mut c_void;
    let rec = Box::new(Recorder::new());
    let p = WaylandAppPresenter::with_abi(rec, SURFACE, APP_DISPLAY).expect("bring-up");
    assert_eq!(p.display, APP_DISPLAY);
    let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();
    assert!(
        !log.iter().any(|r| matches!(r, Rec::GetDisplay(_))),
        "the supplied display must short-circuit wl_proxy_get_display"
    );
    // Everything derives from the SUPPLIED display, not a re-derived one.
    assert_eq!(log[0], Rec::CreateQueue(APP_DISPLAY as usize));
}

/// Without a supplied display AND without `wl_proxy_get_display` (a 1.22 guest), bring-up is a soft
/// NoDisplay — the exact state that latched the presenter unavailable before the display was threaded.
#[test]
fn absent_get_display_without_supplied_display_is_no_display() {
    let mut rec = Recorder::new();
    rec.no_get_display = true;
    let err = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE, core::ptr::null_mut())
        .err()
        .unwrap();
    assert_eq!(err, WlAppError::NoDisplay);
    assert!(err.is_unavailable());
}

/// A null surface pointer (not a wayland app) is a soft NoSurface — the caller keeps its self-owned path.
#[test]
fn null_surface_is_soft_no_surface() {
    let rec = Box::new(Recorder::new());
    let err = WaylandAppPresenter::with_abi(rec, core::ptr::null_mut(), core::ptr::null_mut())
        .err()
        .unwrap();
    assert_eq!(err, WlAppError::NoSurface);
    assert!(err.is_unavailable());
}

/// A too-small readback plane is a hard BadSize (never a silent/blank present).
#[test]
fn short_plane_is_hard_bad_size() {
    let rec = Box::new(Recorder::new());
    let mut p = WaylandAppPresenter::with_abi(rec, SURFACE, core::ptr::null_mut()).expect("bring-up");
    let err = p.present(&[0u8; 4], 4, 4).unwrap_err();
    assert_eq!(err, WlAppError::BadSize);
    assert!(
        !err.is_unavailable(),
        "a live present failure is hard (EGL_CONTEXT_LOST), not a fall-back"
    );
}

/// A constructor returning null (a live marshal failure) is a hard Marshal error.
#[test]
fn null_constructor_is_hard_marshal_error() {
    let mut rec = Recorder::new();
    rec.fail_pool = true;
    let mut p = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE, core::ptr::null_mut()).expect("bring-up");
    let err = p.present(&xrgb(2, 2), 2, 2).unwrap_err();
    assert_eq!(err, WlAppError::Marshal);
    assert!(!err.is_unavailable());
}

/// The soft/hard classification is the exact contract the caller keys its fall-back vs CONTEXT_LOST on.
#[test]
fn error_softness_classification() {
    for e in [
        WlAppError::NoSurface,
        WlAppError::LibraryMissing,
        WlAppError::SymbolMissing("wl_proxy_marshal_flags"),
        WlAppError::NoDisplay,
        WlAppError::QueueSetup,
        WlAppError::NoShmGlobal,
        WlAppError::NoIdentity,
    ] {
        assert!(e.is_unavailable(), "{e:?} must be soft");
    }
    for e in [
        WlAppError::BadSize,
        WlAppError::ShmAlloc,
        WlAppError::Marshal,
        WlAppError::Flush,
    ] {
        assert!(!e.is_unavailable(), "{e:?} must be hard");
    }
}

mod native;

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
