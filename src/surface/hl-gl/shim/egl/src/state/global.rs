use super::*;

thread_local! {
    /// The last EGL error for THIS thread (`eglGetError` reads + clears it). Thread-local because EGL
    /// scopes errors to the calling thread; a process-global error let a Wayland-commit failure on the
    /// compositor thread poison Chrome's raster/GPU threads (they read the same cell and lose their
    /// shared GL context — the whole page then rasterizes black).
    pub(super) static EGL_ERROR: core::cell::Cell<i32> = const { core::cell::Cell::new(EGL_SUCCESS) };
}

// ---- The single process-global `State`, shared across BOTH guest objects --------------------------
//
// The `gl*` (record) and `egl*` (swap/flush) entry points live in SEPARATE shared objects now —
// `libGLESv2.so.2` exports `gl*`, `libEGL.so.1` exports `egl*` (matching real Mesa's split) — yet they
// MUST share ONE `State`: `glDrawArrays` (in libGLESv2) records into the draw-list that `eglSwapBuffers`
// (in libEGL) flushes. So the `State` cannot be a plain per-object `static` (that would give each `.so`
// its OWN copy, and nothing would ever render).
//
// Instead exactly ONE object OWNS the `static` — every build EXCEPT the `cfg(gles_client)` one (so
// libEGL, the EGL/lifecycle object, analogous to Mesa's `libglapi` holding the shared dispatch; and a
// plain host build for this crate's own unit/ABI tests) — and exports the C-ABI accessor
// `hl_shim_state_ptr`. The `cfg(gles_client)` object (libGLESv2) instead has a `DT_NEEDED` on
// `libEGL.so.1` and IMPORTS that accessor, so every `with(..)` on either side resolves to the SAME
// `Mutex<State>`. (No cross-allocator hazard: both objects use Rust's default system allocator = libc
// `malloc`/`free`, one heap for the whole process.)

/// The single owned `State` cell + its exported accessor. Present in every object EXCEPT libGLESv2.
#[cfg(not(gles_client))]
mod owner {
    use super::{Mutex, OnceLock, State};
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    /// Pointer to the process-global `State` mutex — the ONE instance the whole process shares.
    /// Exported (default visibility) so libGLESv2 can bind it via `DT_NEEDED libEGL.so.1`.
    #[no_mangle]
    pub extern "C" fn hl_shim_state_ptr() -> *const Mutex<State> {
        STATE.get_or_init(|| Mutex::new(State::new()))
    }
}

/// The libGLESv2 (non-owner) object imports the accessor from libEGL via `DT_NEEDED`.
#[cfg(gles_client)]
extern "C" {
    fn hl_shim_state_ptr() -> *const Mutex<State>;
}

/// Resolve the process-global `State` mutex — the owner's local cell, or the imported accessor.
pub(super) fn state_ptr() -> *const Mutex<State> {
    #[cfg(gles_client)]
    unsafe {
        hl_shim_state_ptr()
    }
    #[cfg(not(gles_client))]
    owner::hl_shim_state_ptr()
}

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); each entry point does exactly one `with`.
pub struct GlobalState;
/// GBM queries this before advertising the Husklet-private modifier. Until EGL has negotiated an
/// IoSurface-capable executor, GBM must expose only the strict linear fallback.
#[cfg(not(gles_client))]
#[no_mangle]
pub extern "C" fn hl_shim_external_buffers_enabled() -> i32 {
    GlobalState::access(|state| {
        if let Err(error) = state.initialize() {
            if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
                eprintln!("capture ownership boundary=gbm_negotiate result=error error={error}");
            }
            return 0;
        }
        i32::from(state.external_buffers_enabled())
    })
}

/// Process-local bridge used by the CUDA shim to turn a GL name into the host resource capability.
/// Returns zero on success and never exposes the GL name as a cross-connection resource identity.
#[cfg(not(gles_client))]
#[no_mangle]
pub unsafe extern "C" fn hl_gl_export_buffer(name: u32, export_out: *mut u64) -> i32 {
    if name == 0 || export_out.is_null() {
        return -1;
    }
    let result = GlobalState::gpu_io(std::time::Duration::from_secs(31), move |group, sink| {
        let bytes = group.gl.buffers.get(name)
            .ok_or(hl_gpu::GpuError::Invalid("GL buffer"))?.data.as_ref().clone();
        let (buffer, create) = group.gl.interop_buffer_ir(name)?;
        if create {
            use hl_gpu::protocol::model::descriptor::BufferDesc;
            use hl_gpu::protocol::model::enums::buffer_usage;
            use hl_gpu::Cmd;
            sink.submit(&[
                Cmd::CreateBuffer(buffer, BufferDesc {
                    size: bytes.len() as u64,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST | buffer_usage::VERTEX
                        | buffer_usage::INDEX | buffer_usage::STORAGE | buffer_usage::MAP,
                    label: "GL/CUDA interop buffer".into(),
                }),
                Cmd::WriteBuffer { id: buffer, offset: 0, data: bytes },
            ])?;
        }
        sink.export_buffer(hl_gpu::BufferId(buffer))?;
        Ok(())
    });
    match result {
        Ok(result) => match result.observations.last() {
            Some(super::io::Observation::Export(export)) => {
                *export_out = export.0;
                0
            }
            _ => -1,
        },
        Err(_) => -1,
    }
}
