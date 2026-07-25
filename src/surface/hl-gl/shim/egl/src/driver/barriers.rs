// ==================================================================================================
// GLES3.1: memory barriers — ordering hints for image/SSBO/atomic access
// ==================================================================================================

/// `glMemoryBarrier(barriers)` / `glMemoryBarrierByRegion(barriers)` — order incoherent memory accesses
/// (image load/store, SSBO writes, atomic counters) against subsequent access. This deferred model submits
/// each `glDispatchCompute` immediately and materializes no incoherent image/SSBO access between draws that
/// a barrier would need to order, so the ordering is already satisfied — an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMemoryBarrier(_barriers: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMemoryBarrierByRegion(_barriers: u32) {}

// ==================================================================================================
// tests: the per-thread EGL "current" binding + bound-API tracking, and the glGet limit round-trip
//
// These drive the REAL C-ABI entry points (eglMakeCurrent / eglGetCurrent* / eglBindAPI / eglQueryAPI /
// glGetIntegerv) exactly as libepoxy does when GTK brings a GLES context up. The binding is thread-local,
// so each libtest thread starts clean; the tests reset explicitly where they depend on a starting value.
// ==================================================================================================
