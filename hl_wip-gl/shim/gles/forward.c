/*
 * libGLESv2.so.2 — the FORWARDING stub source (intentionally empty).
 *
 * This translation unit defines NO symbols. The top-level `hl_wip-gl/build.rs` links it into
 * `libGLESv2.so.2` with `DT_SONAME=libGLESv2.so.2` and a kept `DT_NEEDED` on `libEGL.so.1`
 * (via `-Wl,--no-as-needed -l:libEGL.so.1`). It is NOT a second implementation: a guest app that
 * `DT_NEEDED`s libGLESv2.so.2 pulls in libEGL, and every `gl*` symbol resolves to the primary libEGL
 * object. Keeping it empty guarantees there is exactly one implementation of each `gl*` entry point.
 */
