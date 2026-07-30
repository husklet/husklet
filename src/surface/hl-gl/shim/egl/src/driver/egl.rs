use super::*;
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglBindAPI(api: u32) -> u32 {
    crate::stub::trace("eglBindAPI", "binding client API");
    if api != EGL_OPENGL_ES_API {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    current::Binding::bind_api(api);
    EGL_TRUE
}

// ==================================================================================================
// EGL: config selection
// ==================================================================================================

/// Write the `n` config handles this driver advertises into the caller's `configs` array (each our
/// single [`CONFIG_TOKEN`]) and report `num_config`. Shared by `eglChooseConfig` / `eglGetConfigs`: the
/// enumeration contract (null array → count only; real array → bounded copy) is decided by
/// [`config::enumerate`], so this only marshals the raw pointers.
unsafe fn write_configs(configs: *mut *mut c_void, config_size: i32, num_config: *mut i32) {
    let (n, num) = config::Config::enumerate(!configs.is_null(), config_size);
    for i in 0..n as isize {
        *configs.offset(i) = CONFIG_TOKEN as *mut c_void;
    }
    if !num_config.is_null() {
        *num_config = num;
    }
}

/// `eglChooseConfig(dpy, attrib_list, configs, config_size, num_config)` — select configs matching the
/// requested attributes. This driver advertises one config that satisfies the common GLES2/ES3 window +
/// pbuffer requests, so a null / empty `attrib_list` (match-all) and a populated list both return it. A
/// null `configs` array is the count-only query (`num_config` = number available). `num_config` is
/// required by the spec; a null one is `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    _attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    crate::stub::trace("eglChooseConfig", "selecting hl configuration");
    if num_config.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    unsafe { write_configs(configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigs(dpy, configs, config_size, num_config)` — enumerate all configs. A null `configs` array
/// returns the total count in `num_config`; a real array is filled with up to `config_size` handles and
/// `num_config` reports how many were written. `num_config` is required; a null one is
/// `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigs(
    _dpy: *mut c_void,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    if num_config.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    unsafe { write_configs(configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigAttrib(dpy, config, attribute, value)` — the value of one attribute of an `EGLConfig`.
/// Delegates the truthful value to [`config::config_attrib`]: a foreign config handle raises
/// `EGL_BAD_CONFIG` and an unrecognized attribute raises `EGL_BAD_ATTRIBUTE` — both return `EGL_FALSE`
/// WITHOUT writing `value` or dereferencing the unknown config, instead of the old silent `0`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigAttrib(
    _dpy: *mut c_void,
    config: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let is_ours = config as usize == CONFIG_TOKEN;
    match config::Config::attrib(is_ours, attribute) {
        Ok(v) => {
            unsafe { *value = v };
            EGL_TRUE
        }
        Err(config::ConfigError::BadConfig) => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
            EGL_FALSE
        }
        Err(config::ConfigError::BadAttribute) => {
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            EGL_FALSE
        }
    }
}

// ==================================================================================================
// EGL: context / surface lifecycle + present
// ==================================================================================================

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateContext(
    dpy: *mut c_void,
    config: *mut c_void,
    share_context: *mut c_void,
    attrib_list: *const i32,
) -> *mut c_void {
    crate::stub::trace("eglCreateContext", "creating GLES context");
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if !config.is_null() && config as usize != CONFIG_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    let attributes = match ContextAttributeList::parse(attrib_list) {
        Ok(attributes) => attributes,
        Err(error @ ContextAttributeError::Malformed { .. }) => {
            error.report();
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            return core::ptr::null_mut();
        }
        Err(error @ ContextAttributeError::Unsupported { .. }) => {
            error.report();
            GlobalState::access(|s| s.set_egl_error(EGL_BAD_MATCH));
            return core::ptr::null_mut();
        }
    };
    let ctx = GlobalState::access(|s| {
        let tok = s.mint_token();
        if !share_context.is_null() {
            let Some(shared) = s.context_attributes(share_context as usize) else {
                s.set_egl_error(EGL_BAD_CONTEXT);
                return core::ptr::null_mut();
            };
            if shared.reset_strategy != attributes.reset_strategy {
                s.set_egl_error(EGL_BAD_MATCH);
                return core::ptr::null_mut();
            }
        }
        tok
    });
    if ctx.is_null() {
        return ctx;
    }
    let share = (!share_context.is_null()).then_some(share_context as usize);
    if !GlobalState::register_context(ctx as usize, attributes, share) {
        GlobalState::access(|state| state.set_egl_error(EGL_BAD_CONTEXT));
        return core::ptr::null_mut();
    }
    hl_log::hl_info!(hl_log::tag::EGL, "eglCreateContext ctx={}", ctx as usize);
    ctx
}

struct ContextAttributeList;
enum ContextAttributeError {
    Malformed {
        reason: String,
        pairs: Vec<(i32, i32)>,
    },
    Unsupported {
        reason: String,
        pairs: Vec<(i32, i32)>,
    },
}

impl ContextAttributeError {
    fn report(&self) {
        let (reason, pairs) = match self {
            Self::Malformed { reason, pairs } | Self::Unsupported { reason, pairs } => {
                (reason.as_str(), pairs.as_slice())
            }
        };
        crate::stub::Diagnostics::context_attributes(reason, pairs);
    }
}

impl ContextAttributeList {
    fn parse(attrib_list: *const i32) -> Result<ContextAttributes, ContextAttributeError> {
        let mut attributes = ContextAttributes {
            client_version: 1,
            minor_version: 0,
            robust_access: false,
            reset_strategy: EGL_NO_RESET_NOTIFICATION_EXT,
            no_error: false,
        };
        if attrib_list.is_null() {
            return Err(ContextAttributeError::Unsupported {
                reason: "null attribute list requests unsupported ES 1.0".into(),
                pairs: Vec::new(),
            });
        }
        let mut pairs = Vec::new();
        for index in 0..32 {
            let attribute = unsafe { *attrib_list.add(index * 2) };
            if attribute == EGL_NONE {
                return if matches!(
                    (attributes.client_version, attributes.minor_version),
                    (2, 0) | (3, 0) | (3, 1)
                ) {
                    Ok(attributes)
                } else {
                    Err(ContextAttributeError::Unsupported {
                        reason: format!(
                            "unsupported GLES version {}.{}",
                            attributes.client_version, attributes.minor_version
                        ),
                        pairs,
                    })
                };
            }
            let value = unsafe { *attrib_list.add(index * 2 + 1) };
            pairs.push((attribute, value));
            match attribute {
                EGL_CONTEXT_CLIENT_VERSION if matches!(value, 1..=3) => {
                    attributes.client_version = value;
                }
                EGL_CONTEXT_MINOR_VERSION_KHR if matches!(value, 0..=1) => {
                    attributes.minor_version = value;
                }
                EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT if matches!(value, 0 | 1) => {
                    attributes.robust_access = value == 1;
                }
                EGL_CONTEXT_OPENGL_NO_ERROR_KHR if matches!(value, 0 | 1) => {
                    attributes.no_error = value == 1;
                }
                EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT
                    if matches!(
                        value,
                        EGL_NO_RESET_NOTIFICATION_EXT | EGL_LOSE_CONTEXT_ON_RESET_EXT
                    ) =>
                {
                    attributes.reset_strategy = value;
                }
                EGL_CONTEXT_CLIENT_VERSION | EGL_CONTEXT_MINOR_VERSION_KHR => {
                    return Err(ContextAttributeError::Malformed {
                        reason: format!(
                            "invalid value 0x{value:04x} for attribute 0x{attribute:04x}"
                        ),
                        pairs,
                    });
                }
                EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT
                | EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT
                | EGL_CONTEXT_OPENGL_NO_ERROR_KHR => {
                    return Err(ContextAttributeError::Malformed {
                        reason: format!(
                            "invalid value 0x{value:04x} for attribute 0x{attribute:04x}"
                        ),
                        pairs,
                    });
                }
                _ => {
                    return Err(ContextAttributeError::Malformed {
                        reason: format!("unknown attribute 0x{attribute:04x}"),
                        pairs,
                    });
                }
            }
        }
        Err(ContextAttributeError::Malformed {
            reason: "attribute list exceeds 32 pairs or lacks EGL_NONE".into(),
            pairs,
        })
    }
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroyContext(dpy: *mut c_void, ctx: *mut c_void) -> u32 {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    if GlobalState::remove_context(ctx as usize) {
        EGL_TRUE
    } else {
        GlobalState::access(|state| state.set_egl_error(EGL_BAD_CONTEXT));
        EGL_FALSE
    }
}

/// `eglMakeCurrent(dpy, draw, read, ctx)` — bind `ctx` (+ its draw/read surfaces + display) as the
/// current binding FOR THE CALLING THREAD, so `eglGetCurrentContext` / `eglGetCurrentDisplay` /
/// `eglGetCurrentSurface` report exactly these on this thread (what libepoxy probes). `ctx ==
/// EGL_NO_CONTEXT` (null) releases the thread's binding. The binding is thread-local (real EGL semantics):
/// another thread keeps its own current context.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglMakeCurrent(
    dpy: *mut c_void,
    draw: *mut c_void,
    read: *mut c_void,
    ctx: *mut c_void,
) -> u32 {
    crate::stub::trace("eglMakeCurrent", "binding GLES context");
    hl_log::hl_info!(
        hl_log::tag::EGL,
        "eglMakeCurrent ctx={} draw={} read={}",
        ctx as usize,
        draw as usize,
        read as usize
    );
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    let release = ctx.is_null() && draw.is_null() && read.is_null();
    if ctx.is_null() && !release {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_MATCH));
        return EGL_FALSE;
    }
    if !ctx.is_null() && draw.is_null() != read.is_null() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_MATCH));
        return EGL_FALSE;
    }
    let previous = current::context();
    let previous_binding = (previous, current::draw_surface(), current::read_surface());
    let result = {
        match GlobalState::bind_current(
            previous_binding,
            (ctx as usize, draw as usize, read as usize),
        ) {
            Ok(()) => EGL_TRUE,
            Err(MakeCurrentError::Context) => {
                GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONTEXT));
                EGL_FALSE
            }
            Err(MakeCurrentError::Surface) => {
                GlobalState::access(|s| s.set_egl_error(EGL_BAD_SURFACE));
                EGL_FALSE
            }
            Err(MakeCurrentError::Access) => {
                GlobalState::access(|s| s.set_egl_error(EGL_BAD_ACCESS));
                EGL_FALSE
            }
        }
    };
    if result == EGL_FALSE {
        return EGL_FALSE;
    }
    current::make_current(dpy as usize, draw as usize, read as usize, ctx as usize);
    EGL_TRUE
}

/// `eglCreateWindowSurface(dpy, config, win, attrib_list)` — bring up the presented default framebuffer.
///
/// `win` is the native window: for a Wayland app it is a `wl_egl_window*` (created by the staged
/// `libwayland-egl.so.1`), from which we read the backing size + the wrapped `wl_surface`. A non-wayland /
/// sizeless window falls back to `$HL_GL_SURFACE_W/_H`. When a `wl_surface` is present and a compositor is
/// reachable (`$WAYLAND_DISPLAY`), a self-contained `wl_shm` present session is brought up so
/// `eglSwapBuffers` shows the frame; otherwise the session stays `None` (present is skipped, never faked).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateWindowSurface(
    dpy: *mut c_void,
    config: *mut c_void,
    win: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    crate::stub::trace("eglCreateWindowSurface", "creating Wayland window surface");
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return core::ptr::null_mut();
    }
    if config as usize != CONFIG_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    WindowSurface::create(win)
}

/// Shared `eglCreateWindowSurface` / `eglCreatePlatformWindowSurface` body: size the surface from the
/// native window and (best-effort) bring up the Wayland present session.
pub(super) struct WindowSurface;

impl WindowSurface {
    pub(super) fn create(win: *mut c_void) -> *mut c_void {
        // Parse the wl_egl_window (or a stock two-int window). A null / sizeless window uses the env default.
        let info = unsafe { hl_gl::adapter::wayland::WlWindowInfo::parse(win) };
        let (width, height) = if win.is_null() {
            default_surface_wh()
        } else {
            (info.width, info.height)
        };
        GlobalState::access(|s| {
            // Compositor bring-up is DEFERRED so a real app window ends up with exactly ONE toplevel:
            //  * a real app `wl_surface` (wl_surface != 0) is presented onto the app's OWN surface at swap
            //    (via the app's libwayland-client) — so the shim must NOT stand up a competing self-owned
            //    toplevel here. The self-owned session is brought up lazily at swap ONLY if that app-surface
            //    presenter proves unavailable.
            //  * a stock / headless / sizeless window (wl_surface == 0) has no app surface to present onto, so
            //    the self-owned `wl_shm` toplevel is the only path — bring it up now (unless suppressed).
            //    `connect_and_handshake` returns None when `$WAYLAND_DISPLAY` is unset or the handshake fails
            //    (an honest "no compositor" — the present is then skipped, never faked).
            if info.wl_surface == 0 && std::env::var_os("HL_GL_NO_WAYLAND").is_none() {
                let geom = hl_gl::adapter::wayland::Geometry::backing(width, height);
                s.wl = hl_gl::adapter::wayland::Wayland::connect_and_handshake(&geom);
            }
            s.create_surface(
                hl_gl::model::context::SurfaceKind::Window,
                width,
                height,
                info.wl_surface,
                win as usize,
            )
        })
    }
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroySurface(dpy: *mut c_void, surface: *mut c_void) -> u32 {
    if dpy as usize != DISPLAY_TOKEN {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_DISPLAY));
        return EGL_FALSE;
    }
    let destroyed = GlobalState::access(|s| {
        if s.destroy_surface(surface as usize) {
            EGL_TRUE
        } else {
            s.set_egl_error(EGL_BAD_SURFACE);
            EGL_FALSE
        }
    });
    if destroyed == EGL_TRUE {
        if let Err(error) = GlobalState::flush_surface_retirements() {
            GlobalState::access(|state| {
                state.set_egl_error(super::egl_error_from_gpu_error(&error))
            });
            return EGL_FALSE;
        }
    }
    destroyed
}

/// `eglSwapBuffers` — the one sink-touching op: lower + submit + present the recorded frame. The
/// AUTHORITATIVE present is the frame-IR submit to the GPU-exec host (`swap::swap_buffers`); only ITS
/// failure fails the swap. The Wayland `wl_shm` commit (onto the app's own `wl_surface`, or the shim's
/// self-owned toplevel) is a best-effort LOCAL display MIRROR of that present: a transient commit failure
/// is logged but does NOT fail `eglSwapBuffers`. This matters because Chrome maps ANY swap failure to a
/// LOST GL CONTEXT and tears down its whole shared-context GPU stack — so a mere present hiccup used to
/// collapse raster and rasterize the entire page black. The compatibility readback and authoritative
/// present are emitted in one submission so the recorded frame is never rendered twice.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSwapBuffers(dpy: *mut c_void, surface: *mut c_void) -> u32 {
    present::swap(dpy as usize, surface as usize)
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSwapInterval(_dpy: *mut c_void, _interval: i32) -> u32 {
    EGL_TRUE
}

/// `eglGetCurrentDisplay()` — the display of the CALLING THREAD's current context (bound by the last
/// `eglMakeCurrent`), or `EGL_NO_DISPLAY` (null) when no context is current on this thread.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentDisplay() -> *mut c_void {
    current::display() as *mut c_void
}

/// `eglGetCurrentContext()` — the context current on the CALLING THREAD, or `EGL_NO_CONTEXT` (null) when
/// none is. libepoxy probes this to select its GL-vs-GLES dispatch table (a NULL here aborts it), so it
/// MUST return the live context on the thread that made it current.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentContext() -> *mut c_void {
    current::context() as *mut c_void
}

/// `eglGetCurrentSurface(readdraw)` — the `EGL_DRAW` (default) or `EGL_READ` surface of the CALLING
/// THREAD's current binding, or `EGL_NO_SURFACE` (null) when no context is current on this thread.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentSurface(readdraw: i32) -> *mut c_void {
    let tok = if readdraw == EGL_READ {
        current::read_surface()
    } else {
        current::draw_surface()
    };
    tok as *mut c_void
}
