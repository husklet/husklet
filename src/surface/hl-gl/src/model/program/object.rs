//! Linked and unlinked program state.

use crate::adapter::glsl::Uni;
use std::collections::BTreeMap;

/// uniform-block/sampler layout.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Program {
    /// Attached vertex/fragment shader GL names.
    pub vs: u32,
    pub fs: u32,
    /// Attached compute shader GL name (`glAttachShader` of a `GL_COMPUTE_SHADER`) — a GLES3.1 compute
    /// program. A program is a compute program when `cs != 0`; it lowers to a `CreateComputePipeline`.
    pub cs: u32,
    pub linked: bool,
    /// Empty after a successful link; otherwise the actionable reason `GL_LINK_STATUS` is false.
    pub link_error: String,
    /// The vertex+fragment GLSL-ES sources captured at link, for the vertex-layout reflection at draw.
    pub vs_src: String,
    pub fs_src: String,
    /// The forwarded vertex/fragment GLSL `Glsl` shader payloads (each a `GlslDescriptor` led by
    /// `GLSL_MAGIC`), lowered to two `CreateShader`s at swap — the host (naga) compiles the source.
    pub vs_ir: Option<Vec<u32>>,
    pub fs_ir: Option<Vec<u32>>,
    /// Attribute locations requested through `glBindAttribLocation` for the next link.
    pub attrib_bindings: BTreeMap<String, u32>,
    /// Active attribute name → location produced by the most recent successful link.
    pub attrib_locations: BTreeMap<String, u32>,
    /// The captured compute GLSL-ES source (for a compute program), translated to `compute_ir` at link.
    pub cs_src: String,
    /// The forwarded compute GLSL `Glsl` shader payload — lowered to a `CreateShader` +
    /// `CreateComputePipeline` at `glDispatchCompute`. `None` for a render (vertex+fragment) program.
    pub compute_ir: Option<Vec<u32>>,
    /// Uniform-block members (name → offset/size) — the layout `glUniform*` writes into `ubuf`.
    pub unis: Vec<Uni>,
    /// The 16-byte-aligned Uniforms struct size (bytes actually shipped as the uniform buffer).
    pub ubuf_size: i32,
    /// The uniform-block bytes written by `glUniform*` (bound at binding 0 when non-empty).
    pub ubuf: Vec<u8>,
    /// Sampler uniform names in declaration order (for the bind-group texture bindings).
    pub samp_names: Vec<String>,
    /// Array length for each sampler declaration (`1` for a non-array sampler).
    pub samp_arrays: Vec<u32>,
    /// Flattened sampler-element index → GL texture unit. Every element starts at unit 0 and may be
    /// reassigned by `glUniform1i[v]`.
    pub samp_units: Vec<i32>,
    /// The program's link generation: bumped on every `glLinkProgram`. The frame builder's program-keyed
    /// shader/pipeline residency cache (`GlContext::program_shader_ir` / `program_pipeline_ir`) keys on this
    /// so a RE-linked program (new shader source / new reflection) gets fresh IR shader modules + pipeline,
    /// while a program that is merely re-used across draws+frames keeps its cached IR ids (created once). `0`
    /// for a never-linked program; the first `glLinkProgram` makes it `1`.
    pub link_gen: u64,
    /// Per flattened sampler-array element, the host bind-group binding INDEX `k` (→ texture `1+2k`,
    /// sampler `2+2k`) the executor's compiled shader declares. For the driver-translated ES2 path this is
    /// the identity (`k == flattened element index`, since the driver EMITS the `layout(binding=)` itself). For the
    /// forward-VERBATIM GskGpu/ANGLE path the host numbers samplers across all preprocessor branches (incl.
    /// the inactive `samplerExternalOES` decls), so `k` diverges from the declaration index and is recovered
    /// by [`crate::adapter::glsl::StageSources::verbatim_sampler_bindings`]. Parallel to `samp_names`;
    /// consumed by the frame builder's
    /// bind-group emission so the driver's texture/sampler bindings match the shader's live layout.
    pub samp_bindings: Vec<u32>,
    /// `GL_DELETE_STATUS`. ES 3.0 §7.3: `glDeleteProgram` on the program that is still the current program
    /// only flags it; it stays current and stays usable until `glUseProgram` moves away from it.
    pub pending_delete: bool,
}
