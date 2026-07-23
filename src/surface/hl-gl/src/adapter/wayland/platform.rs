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

/// Whether `platform` selects the Wayland window system (the only windowed platform this driver backs).
pub struct WaylandPlatform;
impl WaylandPlatform {
    pub fn contains(platform: u32) -> bool {
        platform == EGL_PLATFORM_WAYLAND_KHR
    }
}

/// The CLIENT extension string (`eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS)`): the toolkits probe this
/// BEFORE opening a display to decide whether `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, …)` is
/// usable, so it must advertise the platform-base + wayland-platform extensions. The device family
/// (`EGL_EXT_device_base`/`device_enumeration`/`device_query`) is queryable with `EGL_NO_DISPLAY`
/// (`eglQueryDevicesEXT` / `eglQueryDeviceStringEXT` take no display), so — matching real Mesa — it is
/// advertised in the CLIENT string as well, letting a toolkit's GL loader (e.g. libepoxy for GTK/GDK)
/// resolve `eglQueryDisplayAttribEXT` & friends before display init.
pub fn egl_client_extensions() -> &'static str {
    "EGL_EXT_client_extensions EGL_EXT_platform_base EGL_EXT_platform_wayland EGL_KHR_platform_wayland \
     EGL_EXT_device_base EGL_EXT_device_enumeration EGL_EXT_device_query"
}

/// The per-DISPLAY extension string (`eglQueryString(dpy, EGL_EXTENSIONS)`), advertising the same
/// wayland-platform support plus the context extensions a GLES app expects. `EGL_EXT_device_base` /
/// `EGL_EXT_device_query` are DISPLAY extensions once a display is initialized (GDK's Wayland EGL
/// bring-up requires one of that set to find `eglQueryDisplayAttribEXT`), so they are advertised here too.
pub fn egl_display_extensions() -> &'static str {
    "EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_no_config_context \
     EGL_EXT_platform_wayland EGL_KHR_platform_wayland \
     EGL_EXT_device_base EGL_EXT_device_query"
}
