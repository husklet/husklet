//! Shader object state.

/// One GLES shader object: its kind + source + compile status.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Shader {
    /// `GL_VERTEX_SHADER` / `GL_FRAGMENT_SHADER`.
    pub kind: u32,
    pub src: Option<String>,
    pub compiled: bool,
    /// `GL_DELETE_STATUS`. ES 3.0 §7.1: `glDeleteShader` on a shader still attached to a program only
    /// flags it; the object survives until the last attachment is dropped by `glDetachShader`.
    pub pending_delete: bool,
}
