//! EGL/GL result-code contract: the `EGLint` error values `eglGetError` returns and the `GLenum` codes
//! `glGetError` returns, plus the map from a lowering [`hl_gpu::GpuError`] onto them.
//!
//! Numeric values match Khronos's published `EGL/egl.h` / `GLES2/gl2.h` (the stable ABI a GLES app
//! compiles against); they are re-declared clean-room here, ported from `hl-shim-gl`'s `glconst.rs` +
//! `egl.rs`. Only the subset the hand-written entry points reference is declared — the generated stubs
//! (later pass) return success without inspecting a value. The `GpuError` → code map is what the shim
//! cdylibs (later) will use to turn a lowering error into the error the guest polls for.
//!
//! Mirrors `hl-cuda/src/result.rs` (the CUDA analogue) exactly in shape.

use hl_gpu::GpuError;

// ---- EGLint error codes (returned by eglGetError) -----------------------------------------------
pub const EGL_SUCCESS: i32 = 0x3000;
pub const EGL_NOT_INITIALIZED: i32 = 0x3001;
pub const EGL_BAD_ACCESS: i32 = 0x3002;
pub const EGL_BAD_ALLOC: i32 = 0x3003;
pub const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
pub const EGL_BAD_CONFIG: i32 = 0x3005;
pub const EGL_BAD_CONTEXT: i32 = 0x3006;
pub const EGL_BAD_CURRENT_SURFACE: i32 = 0x3007;
pub const EGL_BAD_DISPLAY: i32 = 0x3008;
pub const EGL_BAD_MATCH: i32 = 0x3009;
pub const EGL_BAD_NATIVE_PIXMAP: i32 = 0x300A;
pub const EGL_BAD_NATIVE_WINDOW: i32 = 0x300B;
pub const EGL_BAD_PARAMETER: i32 = 0x300C;
pub const EGL_BAD_SURFACE: i32 = 0x300D;
pub const EGL_CONTEXT_LOST: i32 = 0x300E;

pub const EGL_FALSE: u32 = 0;
pub const EGL_TRUE: u32 = 1;

// ---- GLenum error codes (returned by glGetError) ------------------------------------------------
pub const GL_NO_ERROR: u32 = 0;
pub const GL_INVALID_ENUM: u32 = 0x0500;
pub const GL_INVALID_VALUE: u32 = 0x0501;
pub const GL_INVALID_OPERATION: u32 = 0x0502;
pub const GL_OUT_OF_MEMORY: u32 = 0x0505;
pub const GL_INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;

/// Map a lowering [`GpuError`] onto the `EGLint` error `eglGetError` reports after a failed frame. A
/// delivery/transport failure at swap is `EGL_CONTEXT_LOST` (the frame could not be presented — matching
/// a real driver losing its context); a bad handle/argument maps to the closest `EGL_BAD_*`.
pub struct EglError(i32);

impl From<&GpuError> for EglError {
    fn from(e: &GpuError) -> Self {
        Self(match e {
            GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => EGL_BAD_SURFACE,
            GpuError::ResourceLimit(_) => EGL_BAD_ALLOC,
            GpuError::Unsupported(_) => EGL_BAD_MATCH,
            GpuError::Invalid(_)
            | GpuError::BadEnum { .. }
            | GpuError::BadTag(_)
            | GpuError::OutOfBounds
            | GpuError::NonFinite(_)
            | GpuError::NonCanonicalBool(_)
            | GpuError::Utf8
            | GpuError::ShortBuffer
            | GpuError::TrailingBytes => EGL_BAD_PARAMETER,
            // A backend PANIC is a backend defect, not a guest error: the frame is refused and the
            // session rolled back, but there is no argument to blame and nothing the app can correct.
            // `EGL_CONTEXT_LOST` is the honest report — the frame could not be presented — and it keeps a
            // driver bug from being mislabelled as bad input from the application.
            GpuError::Panicked(_)
            | GpuError::Kernel(_)
            | GpuError::Decode(_)
            | GpuError::Transport(_) => EGL_CONTEXT_LOST,
        })
    }
}

impl From<EglError> for i32 {
    fn from(value: EglError) -> Self {
        value.0
    }
}

pub const EGL_ERROR_FROM_GPU_ERROR: fn(&GpuError) -> i32 = |error| EglError::from(error).into();
pub use EGL_ERROR_FROM_GPU_ERROR as egl_error_from_gpu_error;

/// Map a lowering [`GpuError`] onto the `GLenum` a GLES entry point would raise via `glGetError`. A
/// resource-limit error is `GL_OUT_OF_MEMORY`; a bad enum/argument is `GL_INVALID_ENUM`/`GL_INVALID_VALUE`.
pub struct GlError(u32);

impl From<&GpuError> for GlError {
    fn from(e: &GpuError) -> Self {
        Self(match e {
            GpuError::ResourceLimit(_) => GL_OUT_OF_MEMORY,
            GpuError::BadEnum { .. } => GL_INVALID_ENUM,
            GpuError::UnknownId { .. } | GpuError::DuplicateId { .. } => GL_INVALID_OPERATION,
            GpuError::Unsupported(_) => GL_INVALID_OPERATION,
            _ => GL_INVALID_VALUE,
        })
    }
}

impl From<GlError> for u32 {
    fn from(value: GlError) -> Self {
        value.0
    }
}

pub const GL_ERROR_FROM_GPU_ERROR: fn(&GpuError) -> u32 = |error| GlError::from(error).into();
pub use GL_ERROR_FROM_GPU_ERROR as gl_error_from_gpu_error;
