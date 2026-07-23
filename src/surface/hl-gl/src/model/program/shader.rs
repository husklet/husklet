//! Shader object state.

/// One GLES shader object: its kind + source + compile status.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Shader {
    /// `GL_VERTEX_SHADER` / `GL_FRAGMENT_SHADER`.
    pub kind: u32,
    pub src: Option<String>,
    pub compiled: bool,
}
