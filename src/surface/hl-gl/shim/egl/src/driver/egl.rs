use super::*;

mod selection;
pub use selection::*;
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
    if !config.is_null() && ConfigHandle::id(config).is_none() {
        GlobalState::access(|s| s.set_egl_error(EGL_BAD_CONFIG));
        return core::ptr::null_mut();
    }
    let attributes = match ContextAttributeList::parse(attrib_list) {
        // The context reports its OWN config's depth/stencil as `GL_DEPTH_BITS`/`GL_STENCIL_BITS`; a null
        // config (`EGL_NO_CONFIG_KHR`) keeps the primary config's.
        Ok(attributes) => ContextAttributes {
            config_id: ConfigHandle::id(config).unwrap_or(config::CONFIG_ID),
            ..attributes
        },
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
        // DEFAULT CONTEXT VERSION — do not change without re-reading this.
        //
        // EGL 1.5 §3.7.1 makes a NULL or immediately-`EGL_NONE` attribute list mean "all defaults", which is
        // legal and common (the spec's own default for `EGL_CONTEXT_MAJOR_VERSION` is 1). This driver ships
        // no ES 1.x pipeline and its only config advertises `EGL_RENDERABLE_TYPE = ES2|ES3`, so an ES 1.x
        // context would have to be refused with `EGL_BAD_MATCH` for lack of a matching config.
        //
        // A client that states no version has expressed no opinion, so it gets the highest COMPLETE profile
        // this driver serves: ES 3.0. Not 2.0 — that answered "what is the lowest the config allows?", a
        // different question, and it strands a toolkit that probes the default context and needs ES 3 (GTK4's
        // GSK renderer refuses anything below ES 3.0 and falls back to Cairo over `wl_shm`). Not 3.1 either:
        // `GL_MAX_IMAGE_UNITS` is 0 and `glBindImageTexture` is an honest no-op, so the ES 3.1 compute/image
        // requirements are not met and 3.1 is only served to a client that explicitly asks for it.
        //
        // An EXPLICIT version request is still honoured exactly, so Chrome/ANGLE (which requests ES 2.0 and
        // ES 3.0) and Dawn (ES 3.1) observe an unchanged identity — see
        // `chrome_es30_es20_and_dawn_es31_requests_report_the_selected_profile`.
        let mut attributes = ContextAttributes {
            client_version: 3,
            minor_version: 0,
            robust_access: false,
            reset_strategy: EGL_NO_RESET_NOTIFICATION_EXT,
            no_error: false,
            config_id: config::CONFIG_ID,
        };
        if attrib_list.is_null() {
            return Ok(attributes);
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
                | EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR
                    if matches!(
                        value,
                        EGL_NO_RESET_NOTIFICATION_EXT | EGL_LOSE_CONTEXT_ON_RESET_EXT
                    ) =>
                {
                    attributes.reset_strategy = value;
                }
                // `EGL_KHR_create_context`: the flags word, whose default is 0. A client passing it — GDK
                // and every other libepoxy-based toolkit do — must not have the whole context refused. The
                // debug bit is a hint this driver has no debug-output extension to honour, so it is
                // recorded and ignored; the robust-access bit means what the EXT attribute means; the
                // forward-compatible bit is defined for OpenGL only and is an error on an ES context.
                EGL_CONTEXT_FLAGS_KHR
                    if value
                        & !(EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR
                            | EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR)
                        == 0 =>
                {
                    attributes.robust_access |=
                        value & EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR != 0;
                }
                EGL_CONTEXT_FLAGS_KHR
                    if value & EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR != 0 =>
                {
                    return Err(ContextAttributeError::Malformed {
                        reason: "EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR is OpenGL-only"
                            .into(),
                        pairs,
                    });
                }
                EGL_CONTEXT_CLIENT_VERSION
                | EGL_CONTEXT_MINOR_VERSION_KHR
                | EGL_CONTEXT_FLAGS_KHR
                | EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR => {
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
    if ConfigHandle::id(config).is_none() {
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
