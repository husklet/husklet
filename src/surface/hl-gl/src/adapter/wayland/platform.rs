// ==================================================================================================
// EGL platform recognition + advertised extensions
// ==================================================================================================

/// `EGL_PLATFORM_WAYLAND_KHR` (== `EGL_PLATFORM_WAYLAND_EXT`) — the `platform` a Wayland app passes to
/// `eglGetPlatformDisplay` so the driver knows the native display is a `wl_display*`.
pub const EGL_PLATFORM_WAYLAND_KHR: u32 = 0x31D8;
/// `EGL_PLATFORM_WAYLAND_EXT` — the `EGL_EXT_platform_base` spelling (same numeric value as the KHR one).
pub const EGL_PLATFORM_WAYLAND_EXT: u32 = 0x31D8;
/// `EGL_PLATFORM_GBM_KHR` — recognised so a GBM probe gets a truthful "not wayland" answer.
pub const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;
/// `EGL_PLATFORM_SURFACELESS_MESA` — a display that renders without a window system. The native display
/// is always `EGL_DEFAULT_DISPLAY`; every surface is a pbuffer or the surfaceless default framebuffer,
/// which is exactly the offscreen path this driver already serves.
pub const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
/// `EGL_PLATFORM_DEVICE_EXT` — a display on an `EGLDeviceEXT` this driver already enumerates through
/// `eglQueryDevicesEXT`. The platform defines NO native window or pixmap type at all, so a display of it
/// promises exactly what this driver already delivers: pbuffer and surfaceless rendering on the one
/// device backed by Husklet's projected render node. Chrome's GPU process asks for this display when
/// GBM is unavailable, passing the very device token our own enumeration handed it.
pub const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313F;

/// Whether `platform` selects the Wayland window system (the only windowed platform this driver backs).
pub struct WaylandPlatform;
impl WaylandPlatform {
    pub fn contains(platform: u32) -> bool {
        platform == EGL_PLATFORM_WAYLAND_KHR
    }
}

/// Whether this driver can back a display for `platform` at all.
///
/// A platform this driver cannot serve must be REFUSED at `eglGetPlatformDisplay`, not accepted and
/// silently treated as Wayland: a caller that gets a display back is entitled to a native surface of
/// that platform's type, and there is none. Accepting every enum and answering Wayland is how a GBM or
/// X11 caller got a display it could never present through, and found out only frames later.
pub struct SupportedPlatform;
impl SupportedPlatform {
    pub fn contains(platform: u32) -> bool {
        matches!(
            platform,
            EGL_PLATFORM_WAYLAND_KHR | EGL_PLATFORM_SURFACELESS_MESA | EGL_PLATFORM_DEVICE_EXT
        )
    }
}

/// The CLIENT extension string (`eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS)`): the toolkits probe this
/// BEFORE opening a display to decide whether `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, …)` is
/// usable, so it must advertise the platform-base + wayland-platform extensions. The device family
/// (`EGL_EXT_device_base`/`device_enumeration`/`device_query`) is queryable with `EGL_NO_DISPLAY`
/// (`eglQueryDevicesEXT` / `eglQueryDeviceStringEXT` take no display), so — matching real Mesa — it is
/// advertised in the CLIENT string as well, letting a toolkit's GL loader (e.g. libepoxy for GTK/GDK)
/// resolve `eglQueryDisplayAttribEXT` & friends before display init.
///
/// `EGL_MESA_platform_surfaceless` belongs here too: this string is read BEFORE any display call, and a
/// caller that does not find its platform named here never calls the driver at all. ANGLE was measured
/// resolving every entry point from this driver and then reporting "Failed to get system egl display"
/// without invoking one of them, because the platform it wanted was not in this list.
///
/// `EGL_EXT_platform_device` is here for the same reason, and is what Chrome's GPU process actually
/// needs: an interposer inside that process shows ANGLE asking for `EGL_PLATFORM_GBM_KHR` first and then
/// falling back to `EGL_PLATFORM_DEVICE_EXT` with the device `eglQueryDevicesEXT` returned. With neither
/// name in this string it made both requests only in the measurement (which patched the string) and
/// otherwise gave up before calling the driver at all.
pub fn egl_client_extensions() -> &'static str {
    "EGL_EXT_client_extensions EGL_EXT_platform_base EGL_EXT_platform_wayland EGL_KHR_platform_wayland \
     EGL_MESA_platform_surfaceless EGL_EXT_platform_device \
     EGL_EXT_device_base EGL_EXT_device_enumeration EGL_EXT_device_query"
}

/// The per-DISPLAY extension string (`eglQueryString(dpy, EGL_EXTENSIONS)`), advertising the same
/// wayland-platform support plus the context extensions a GLES app expects. `EGL_EXT_device_base` /
/// `EGL_EXT_device_query` are DISPLAY extensions once a display is initialized (GDK's Wayland EGL
/// bring-up requires one of that set to find `eglQueryDisplayAttribEXT`), so they are advertised here too.
pub fn egl_display_extensions() -> &'static str {
    "EGL_KHR_create_context EGL_KHR_create_context_no_error \
     EGL_KHR_surfaceless_context EGL_KHR_no_config_context \
     EGL_EXT_create_context_robustness EGL_KHR_fence_sync \
     EGL_KHR_image_base \
     EGL_EXT_image_dma_buf_import EGL_EXT_image_dma_buf_import_modifiers \
     EGL_EXT_platform_wayland EGL_KHR_platform_wayland EGL_MESA_platform_surfaceless \
     EGL_EXT_platform_device \
     EGL_EXT_device_base EGL_EXT_device_query"
}
