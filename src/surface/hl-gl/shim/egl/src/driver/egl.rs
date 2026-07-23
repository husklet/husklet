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
    _dpy: *mut c_void,
    _config: *mut c_void,
    _share_context: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    crate::stub::trace("eglCreateContext", "creating GLES context");
    let ctx = GlobalState::access(|s| {
        let tok = s.mint_token();
        s.create_context();
        tok
    });
    hl_log::hl_info!(hl_log::tag::EGL, "eglCreateContext ctx={}", ctx as usize);
    ctx
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroyContext(_dpy: *mut c_void, ctx: *mut c_void) -> u32 {
    // If this context is current on the calling thread, releasing it drops the thread's binding.
    current::Binding::release_if_context(ctx as usize);
    // Account the teardown; destroying the LAST live context retires the shared model's whole working set so
    // its host residency is refunded (the fix for Chrome's lost-context accumulation — see `destroy_context`).
    GlobalState::access(|s| s.destroy_context());
    EGL_TRUE
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
    current::make_current(dpy as usize, draw as usize, read as usize, ctx as usize);
    // A successful eglMakeCurrent resets the calling thread's EGL error to EGL_SUCCESS (EGL 1.5 §3.1).
    // Without this, a stale EGL_CONTEXT_LOST left by an earlier failed present would make Chrome treat a
    // freshly-bound, valid context as lost the next time it polls eglGetError — collapsing every shared
    // context and rasterizing the whole page black.
    GlobalState::access(|s| s.clear_egl_error());
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
    _dpy: *mut c_void,
    _config: *mut c_void,
    win: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    crate::stub::trace("eglCreateWindowSurface", "creating Wayland window surface");
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
            s.ctx.surf = GlSurface {
                have: true,
                width,
                height,
            };
            s.current_is_wayland = info.wl_surface != 0;
            s.wl_surface_ptr = info.wl_surface;
            // The surface token is not "current" until eglMakeCurrent binds it (real EGL): the per-thread
            // current-surface binding is set there, never here.
            let tok = s.mint_token();
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
            tok
        })
    }
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroySurface(_dpy: *mut c_void, surface: *mut c_void) -> u32 {
    // Drop it from the calling thread's current binding (if it was the draw/read surface).
    current::Binding::forget_surface(surface as usize);
    GlobalState::access(|s| {
        s.ctx.surf.have = false;
    });
    EGL_TRUE
}

/// `eglSwapBuffers` — the one sink-touching op: lower + submit + present the recorded frame. The
/// AUTHORITATIVE present is the frame-IR submit to the GPU-exec host (`swap::swap_buffers`); only ITS
/// failure fails the swap. The Wayland `wl_shm` commit (onto the app's own `wl_surface`, or the shim's
/// self-owned toplevel) is a best-effort LOCAL display MIRROR of that present: a transient commit failure
/// is logged but does NOT fail `eglSwapBuffers`. This matters because Chrome maps ANY swap failure to a
/// LOST GL CONTEXT and tears down its whole shared-context GPU stack — so a mere present hiccup used to
/// collapse raster and rasterize the entire page black. The read-back happens BEFORE the swap (which
/// resets the draw-list).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSwapBuffers(_dpy: *mut c_void, _surface: *mut c_void) -> u32 {
    crate::stub::trace("eglSwapBuffers", "presenting frame");
    GlobalState::access(|s| {
        let _sp = hl_log::hl_span!(hl_log::tag::PRESENT, "swap");
        hl_log::hl_debug!(
            hl_log::tag::PRESENT,
            "eglSwapBuffers {}x{}",
            s.ctx.surf.width,
            s.ctx.surf.height
        );
        // Two present targets share the SAME read-back frame:
        //  * the app's OWN `wl_surface` (the real-window path) — when this is a Wayland window that
        //    carried an app `wl_surface*`, or
        //  * the shim's self-owned `wl_shm` toplevel (`s.wl`) — the fallback / headless path.
        let is_app_surface = s.current_is_wayland && s.wl_surface_ptr != 0;
        let want_readback = is_app_surface || s.wl.is_some();

        // Read the rendered frame back (draws intact) BEFORE the swap resets the draw-list.
        let wl_pixels = if want_readback {
            let (w, h) = (s.ctx.surf.width as i32, s.ctx.surf.height as i32);
            readpixels::read_pixels(&mut s.ctx, &mut s.sink, 0, 0, w, h, GL_RGBA)
                .ok()
                .map(|rgba| {
                    hl_gl::adapter::wayland::rgba_to_xrgb8888(&rgba, w as usize, h as usize)
                })
        } else {
            None
        };

        // The authoritative present: lower + submit the frame IR (+ Present) to the GPU-exec host.
        if let Err(e) = swap::swap_buffers(&mut s.ctx, &mut s.sink) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
            return EGL_FALSE;
        }

        // Commit the read-back frame to the compositor (a failure is surfaced, never a silent present).
        if let Some(px) = wl_pixels {
            let (w, h) = (s.ctx.surf.width, s.ctx.surf.height);

            // Prefer presenting onto the app's OWN wl_surface (marshalled via the app's libwayland-client).
            if is_app_surface {
                match s.present_to_app_surface(&px, w, h) {
                    AppPresentOutcome::Presented => return EGL_TRUE,
                    // A commit/flush to the app's wl_surface failed. This is a display-delivery miss, NOT a
                    // lost GL context: the authoritative present already happened above (`swap::swap_buffers`
                    // submitted the frame IR to the GPU-exec host). Do NOT fail eglSwapBuffers — Chrome maps
                    // ANY swap failure (EGL_FALSE, or a non-SUCCESS eglGetError) to a LOST GL CONTEXT and
                    // tears down its whole shared-context GPU stack, after which every raster MakeCurrent
                    // fails and the page rasterizes black. Log the miss and report success; the next swap
                    // re-presents the (retained) frame.
                    AppPresentOutcome::Failed => {
                        hl_log::hl_warn!(hl_log::tag::PRESENT, "present-to-app-surface failed {}x{} — best-effort skip, authoritative present already done", w, h);
                        return EGL_TRUE;
                    }
                    // Presenter unavailable (not a wayland app / libwayland or a symbol/global absent):
                    // fall through to the self-owned present below.
                    AppPresentOutcome::Unavailable => {}
                }
            }

            // Fallback: the shim's self-owned `wl_shm` toplevel. For a deferred app surface whose presenter
            // turned out unavailable, the self-owned session is brought up lazily now (a stock/headless
            // window already brought one up in `eglCreateWindowSurface`).
            if s.wl.is_none() && std::env::var_os("HL_GL_NO_WAYLAND").is_none() {
                let geom = hl_gl::adapter::wayland::Geometry::backing(w, h);
                s.wl = hl_gl::adapter::wayland::Wayland::connect_and_handshake(&geom);
            }
            let geom = hl_gl::adapter::wayland::Geometry::backing(w, h);
            if let Some(wl) = s.wl.as_mut() {
                if let Err(e) = wl.commit(&px, &geom) {
                    // The self-owned wl_shm toplevel is a best-effort LOCAL MIRROR of the authoritative
                    // present that already succeeded above (`swap::swap_buffers` submitted + presented the
                    // frame IR to the GPU-exec host). A transient commit failure of this mirror (compositor
                    // pacing, or a second surface racing the app's own wl_surface) must NOT fail
                    // eglSwapBuffers: Chrome maps ANY swap failure to a LOST GL CONTEXT and tears down its
                    // entire shared-context GPU stack, after which every raster MakeCurrent fails and the
                    // whole page rasterizes black. Log the miss and report the swap as succeeded — the
                    // authoritative GPU present did happen; only this dev-compositor mirror was skipped.
                    hl_log::hl_warn!(hl_log::tag::PRESENT, "self-owned wl_shm mirror commit failed ({e:?}) — best-effort skip, authoritative present already done");
                }
            }
        }
        EGL_TRUE
    })
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
