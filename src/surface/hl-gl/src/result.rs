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

use hl_gpu::transport::model::header::RefusalKind;
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
/// The share group has been terminated and every object in it is gone (`GL_KHR_robustness`, and core
/// since ES 3.2). Reported once per loss, because an application that drains the error queue in a loop
/// would otherwise never leave it.
pub const GL_CONTEXT_LOST: u32 = 0x0507;

// ---- GLenum graphics-reset status (returned by glGetGraphicsResetStatus) ------------------------
/// The context was reset for a reason the driver cannot attribute to this context or another one
/// (`GL_KHR_robustness`). A lost share group is exactly that: the transport failed, and which context's
/// work provoked it is not recoverable afterwards.
pub const GL_UNKNOWN_CONTEXT_RESET: u32 = 0x8255;

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
            // `EGL_BAD_ACCESS` is the one EGL code that actually means contention — a resource already
            // in use by another thread or context — so the timing distinction survives here rather than
            // collapsing into `EGL_BAD_PARAMETER` with the malformed-input arms below.
            GpuError::MappedElsewhere { .. } => EGL_BAD_ACCESS,
            GpuError::Invalid(_)
            | GpuError::BadEnum { .. }
            | GpuError::BadTag(_)
            | GpuError::OutOfBounds
            | GpuError::NonFinite(_)
            | GpuError::NonCanonicalBool(_)
            | GpuError::Utf8
            | GpuError::ShortBuffer
            | GpuError::TrailingBytes => EGL_BAD_PARAMETER,
            // A host that received a complete request and REFUSED it has not lost anything: the batch was
            // rejected atomically, the connection is still there, and the share group is not retired (see
            // `GlobalState::retires_share_group`). Reporting `EGL_CONTEXT_LOST` for it would tell a robust
            // application to destroy its context and rebuild its entire working set over one bad frame,
            // which is the same amplification the group-loss escalation caused, moved up a layer.
            //
            // The acknowledgement now carries the CLASS of the refusal, so this is the same code the guest
            // would have reported had it raised the error itself — an exact answer rather than the nearest
            // legal one. `Unstated` is the only approximation left, and it is the honest one: the host
            // declined and named no reason, the display, context and surface are all still valid.
            GpuError::Transport(failure) if failure.refusal() => match failure.refusal_kind() {
                Some(RefusalKind::ResourceLimit) => EGL_BAD_ALLOC,
                Some(RefusalKind::Unsupported) => EGL_BAD_MATCH,
                Some(RefusalKind::UnknownId) => EGL_BAD_SURFACE,
                Some(RefusalKind::MappedElsewhere) => EGL_BAD_ACCESS,
                // `Kernel` is grouped with `Invalid` deliberately: before the class existed, a shader the
                // host could not lower arrived here as `Invalid`, and GL has no distinct code for
                // "unlowerable program" at this boundary the way CUDA's `CUDA_ERROR_INVALID_PTX` does.
                // Keeping the grouping leaves GL's reported codes bit-identical.
                Some(RefusalKind::Invalid)
                | Some(RefusalKind::Kernel)
                | Some(RefusalKind::OutOfBounds) => EGL_BAD_PARAMETER,
                Some(RefusalKind::Unstated) | None => EGL_BAD_ACCESS,
            },
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
            // The host refused this request and the connection survived it, so the call — not the context
            // — failed, and the acknowledgement's class says which way. Each arm is the code this driver
            // raises for the same condition locally, so where the error was detected stops being visible
            // to the application. An unclassified refusal keeps `GL_INVALID_OPERATION`: the operation
            // could not be performed, which is all the host actually said.
            GpuError::Transport(failure) if failure.refusal() => match failure.refusal_kind() {
                Some(RefusalKind::ResourceLimit) => GL_OUT_OF_MEMORY,
                // GL has no contention code; INVALID_OPERATION is the closest honest one — the call was
                // not legal in the current state, which is exactly true while another connection holds
                // the map. Unlike Vulkan this loses little: it is also what a real GL raises for using a
                // buffer that is mapped.
                Some(RefusalKind::MappedElsewhere) => GL_INVALID_OPERATION,
                Some(RefusalKind::Invalid)
                | Some(RefusalKind::Kernel)
                | Some(RefusalKind::OutOfBounds) => GL_INVALID_VALUE,
                Some(RefusalKind::Unsupported)
                | Some(RefusalKind::UnknownId)
                | Some(RefusalKind::Unstated)
                | None => GL_INVALID_OPERATION,
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::{TransportError, TransportPhase};

    fn rejected() -> GpuError {
        GpuError::Transport(TransportError::Rejected {
            phase: TransportPhase::Acknowledgement,
            acknowledgement: 0,
        })
    }

    fn gone() -> GpuError {
        GpuError::Transport(TransportError::Unavailable {
            phase: TransportPhase::FrameWrite,
            detail: "peer closed".into(),
        })
    }

    fn refused_as(kind: RefusalKind) -> GpuError {
        GpuError::Transport(TransportError::Rejected {
            phase: TransportPhase::Acknowledgement,
            acknowledgement: kind.ack(),
        })
    }

    /// A refusal and a dead transport are both `GpuError::Transport`, and they must not report the same
    /// thing. Telling an application its context is lost makes it destroy the context and rebuild its
    /// whole working set — the right response to a transport that is gone, and a catastrophic
    /// over-reaction to one frame the host declined to run.
    #[test]
    fn a_refusal_is_an_ordinary_error_and_a_dead_transport_is_a_lost_context() {
        assert_eq!(i32::from(EglError::from(&rejected())), EGL_BAD_ACCESS);
        assert_eq!(u32::from(GlError::from(&rejected())), GL_INVALID_OPERATION);

        assert_eq!(i32::from(EglError::from(&gone())), EGL_CONTEXT_LOST);
    }

    /// A classified refusal reports the code this driver raises for the SAME condition locally, so where
    /// the error was detected stops being visible to the application — a host that ran out of memory and
    /// a guest that did are the same `GL_OUT_OF_MEMORY`. The unstated class is the only approximation
    /// left, and it must not silently absorb the classified ones.
    #[test]
    fn a_classified_refusal_reports_what_the_same_failure_would_report_locally() {
        for (kind, egl, gl) in [
            (RefusalKind::ResourceLimit, EGL_BAD_ALLOC, GL_OUT_OF_MEMORY),
            (
                RefusalKind::Unsupported,
                EGL_BAD_MATCH,
                GL_INVALID_OPERATION,
            ),
            (
                RefusalKind::UnknownId,
                EGL_BAD_SURFACE,
                GL_INVALID_OPERATION,
            ),
            (RefusalKind::Invalid, EGL_BAD_PARAMETER, GL_INVALID_VALUE),
            (
                RefusalKind::OutOfBounds,
                EGL_BAD_PARAMETER,
                GL_INVALID_VALUE,
            ),
            (RefusalKind::Unstated, EGL_BAD_ACCESS, GL_INVALID_OPERATION),
        ] {
            let error = refused_as(kind);
            assert_eq!(i32::from(EglError::from(&error)), egl, "{kind:?} EGL code");
            assert_eq!(u32::from(GlError::from(&error)), gl, "{kind:?} GL code");
        }
    }
}
