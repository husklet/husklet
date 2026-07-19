//! GL shader + program objects, the shader/program table, and the recorded-draw types.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Shader`, `Program`, `Attr`, `DrawCall`). A shader keeps its
//! GLSL-ES source; a program keeps its attached vertex+fragment sources and, once linked, the
//! translated shader-IR ([`adapter::glsl`](crate::adapter::glsl)) + uniform-block layout + sampler
//! bindings. GL is deferred-lowering, so a [`DrawCall`] is the immutable snapshot of the bound draw
//! state at the moment `glDrawArrays`/`glDrawElements` records it; the frame builder replays the
//! draw-list into IR at swap.

use crate::adapter::glsl::{self, Uni};
use hl_gpu::protocol::model::enums::TextureFormat;
use std::collections::HashMap;

/// The vertex-attribute upper bound GL exposes (matches `hl-shim-gl` `MAXATTR`).
pub const MAX_ATTR: usize = 16;

/// One GLES shader object: its kind + source + compile status.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Shader {
    /// `GL_VERTEX_SHADER` / `GL_FRAGMENT_SHADER`.
    pub kind: u32,
    pub src: Option<String>,
    pub compiled: bool,
}

/// One GLES program object: the attached shaders and, once linked, the translated shader-IR + reflected
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
    /// The vertex+fragment GLSL-ES sources captured at link, for the vertex-layout reflection at draw.
    pub vs_src: String,
    pub fs_src: String,
    /// The forwarded vertex/fragment GLSL `Glsl` shader payloads (each a `GlslDescriptor` led by
    /// `GLSL_MAGIC`), lowered to two `CreateShader`s at swap — the host (naga) compiles the source.
    pub vs_ir: Option<Vec<u32>>,
    pub fs_ir: Option<Vec<u32>>,
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
    /// Sampler-uniform index → GL texture unit (set by `glUniform1i`).
    pub samp_units: [i32; 4],
    /// The program's link generation: bumped on every `glLinkProgram`. The frame builder's program-keyed
    /// shader/pipeline residency cache (`GlContext::program_shader_ir` / `program_pipeline_ir`) keys on this
    /// so a RE-linked program (new shader source / new reflection) gets fresh IR shader modules + pipeline,
    /// while a program that is merely re-used across draws+frames keeps its cached IR ids (created once). `0`
    /// for a never-linked program; the first `glLinkProgram` makes it `1`.
    pub link_gen: u64,
    /// Per-`samp_names` entry, the host bind-group binding INDEX `k` (→ texture `1+2k`, sampler `2+2k`) the
    /// executor's compiled shader declares for that sampler. For the driver-translated ES2 path this is the
    /// identity (`k == declaration index`, since the driver EMITS the `layout(binding=)` itself). For the
    /// forward-VERBATIM GskGpu/ANGLE path the host numbers samplers across all preprocessor branches (incl.
    /// the inactive `samplerExternalOES` decls), so `k` diverges from the declaration index and is recovered
    /// by [`glsl::verbatim_sampler_bindings`]. Parallel to `samp_names`; consumed by the frame builder's
    /// bind-group emission so the driver's texture/sampler bindings match the shader's live layout.
    pub samp_bindings: Vec<u32>,
}

impl Program {
    /// `glLinkProgram` — translate the attached shaders to shader-IR and reflect the layout.
    ///
    /// A **render** program (a vertex+fragment pair) translates the GLSL-ES pair to combined MSL
    /// (`shader_ir`) and reflects the uniform-block + sampler layout. A **compute** program (`cs_src`
    /// non-empty) translates the compute source to `compute_ir`, which `glDispatchCompute` lowers to a
    /// `CreateShader` + `CreateComputePipeline`.
    pub fn link(&mut self, vs_src: String, fs_src: String, cs_src: String) {
        use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
        // A (re)link produces fresh shader IR + reflection; bump the generation so the frame builder's
        // program-keyed shader/pipeline cache invalidates any IR it created for the previous link.
        self.link_gen += 1;
        if !cs_src.is_empty() {
            let source = glsl::Translator::compute(&cs_src);
            self.compute_ir = Some(
                GlslDescriptor {
                    stage: glsl_stage::COMPUTE,
                    entry: "cmain".into(),
                    source,
                }
                .to_words(),
            );
            self.cs_src = cs_src;
            self.linked = true;
            return;
        }
        let (unis, ubuf_size) = glsl::StageSources::new(&vs_src, &fs_src).uniform_layout();
        self.samp_names = glsl::StageSources::new(&vs_src, &fs_src).samplers();
        // GskGpu (GTK4 "gl") / ANGLE (Chrome) source uses helper functions taking combined sampler
        // parameters and `gl_VertexID` vertex-pulling — constructs `translate_render`'s reflect-and-
        // regenerate would destroy (it keeps only `main` and reflects a flat ES2 interface). For such
        // source, forward the stage VERBATIM so the host executor's ES route (`glsl_es` +
        // `spirv_split`-style combined→separate sampler split) compiles the REAL text. The sampler /
        // uniform reflection above still drives the bind-group layout, which stays on the shared
        // `binding = 1+2k / 2+2k` scheme the executor emits. Simple ES2 shaders take `translate_render`.
        let verbatim = glsl::Source::new(&vs_src).is_forward_verbatim()
            || glsl::Source::new(&fs_src).is_forward_verbatim();
        let (vs_glsl, fs_glsl) = if verbatim {
            hl_log::hl_debug!(
                hl_log::tag::GL,
                "link: GskGpu/ANGLE-shaped GLSL-ES → forward verbatim to host ES route"
            );
            // naga's `glsl-in` rejects GLSL-ES's implicit default uniform block AND any bindingless uniform
            // interface block ("uniform/buffer blocks require layout(binding=X)"). Chrome's Skia GPU-raster
            // GLSL declares BARE default-block uniforms (`uniform highp vec4 sk_RTAdjust;`); wrap those into a
            // binding-0 `HlUniforms` std140 block (using the combined cross-stage layout so it matches the
            // `Program::ubuf` bytes the frame builder binds at binding 0) and inject bindings into any explicit
            // block that lacks one. GskGpu's already-bound `binding = 0` block path stays byte-identical.
            // Combined samplers are left untouched for the host's `split_global_samplers`. naga ALSO rejects
            // Skia's BARE (bindingless) vertex inputs / varyings / outputs (`in highp vec4 fillBounds;
            // flat out mediump vec4 vcolor_S0;`) — it collapses every one to location 0
            // (`BindingCollision`) — so inject `layout(location = N)` across BOTH stages (name-matching a
            // vertex `out` to the fragment `in` varying). GskGpu's `IN()`/`PASS()` macro varyings already
            // carry a location, so its stages stay byte-identical.
            let combined = glsl::StageSources::new(&vs_src, &fs_src).uniform_decls();
            glsl::prepare_verbatim_program(&vs_src, &fs_src, &combined)
        } else {
            glsl::StageSources::new(&vs_src, &fs_src).translate_render()
        };
        // Bind-group binding index per sampler. The ES2 path EMITS its own `layout(binding=)` in declaration
        // order, so `k == index`; the verbatim path is numbered by the HOST across all preprocessor branches
        // (incl. inactive `samplerExternalOES` decls), so `k` is recovered from the forwarded fragment text.
        self.samp_bindings = if verbatim {
            glsl::StageSources::new("", &fs_src).verbatim_sampler_bindings(&self.samp_names)
        } else {
            (0..self.samp_names.len() as u32).collect()
        };
        self.vs_ir = Some(
            GlslDescriptor {
                stage: glsl_stage::VERTEX,
                entry: "vmain".into(),
                source: vs_glsl,
            }
            .to_words(),
        );
        self.fs_ir = Some(
            GlslDescriptor {
                stage: glsl_stage::FRAGMENT,
                entry: "fmain".into(),
                source: fs_glsl,
            }
            .to_words(),
        );
        self.unis = unis;
        self.ubuf_size = ubuf_size;
        self.ubuf = vec![0u8; ubuf_size.max(0) as usize];
        self.vs_src = vs_src;
        self.fs_src = fs_src;
        self.linked = true;
    }

    /// Is this a GLES3.1 compute program (a linked `GL_COMPUTE_SHADER`)?
    pub fn is_compute(&self) -> bool {
        self.compute_ir.is_some()
    }

    /// Does this program declare any data uniforms (→ a uniform buffer + binding 0)?
    pub fn has_uniforms(&self) -> bool {
        !self.unis.is_empty()
    }

    /// `glGetUniformLocation(name)` — resolve `name` to the location the `glUniform*` recording ops key
    /// on: the uniform's DECLARATION INDEX in its reflected table (`unis` for a data uniform, `samp_names`
    /// for a sampler uniform), matching the index [`crate::service::record::uniform_at`] /
    /// [`crate::service::record::uniform_sampler`] expect. Returns `-1` if the name is not an active
    /// uniform (unlinked program, or a name the reflection did not find). Data uniforms are searched
    /// first, then samplers.
    pub fn uniform_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        if let Some(i) = self.unis.iter().position(|u| u.name == name) {
            return i as i32;
        }
        if let Some(i) = self.samp_names.iter().position(|s| s == name) {
            return i as i32;
        }
        -1
    }

    /// `glGetAttribLocation(name)` — the attribute's declaration-order index in the vertex shader, which
    /// is exactly the `[[attribute(L)]]` slot the translator emits (so it matches the index a
    /// `glVertexAttribPointer(L, …)` binds). Returns `-1` for an unknown attribute or an unlinked program.
    pub fn attrib_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        crate::adapter::glsl::Source::new(&self.vs_src)
            .vertex_attrs()
            .iter()
            .position(|a| a.name == name)
            .map(|i| i as i32)
            .unwrap_or(-1)
    }
}

/// One vertex-attribute pointer's bound state (`glVertexAttribPointer` + enable flag).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Attr {
    pub enabled: bool,
    /// Component count (1..4).
    pub size: i32,
    pub normalized: bool,
    pub integer: bool,
    /// The GL component type enum (`GL_FLOAT`, `GL_UNSIGNED_BYTE`, …).
    pub kind: u32,
    pub stride: i32,
    pub offset: usize,
    /// The GL array-buffer name this attribute fetches from.
    pub buffer: u32,
    /// The instance-step divisor (`glVertexAttribDivisor`): `0` = advance per vertex, `>0` = per
    /// instance. This model has a single step rate per vertex-buffer slot, so a non-zero divisor marks
    /// the slot instance-stepped (the exact rate `N>1` is not modeled — see `service::frame`).
    pub divisor: u32,
}

/// A captured **client-side vertex array**: one enabled attribute drawn with NO vertex buffer object
/// bound (`Attr::buffer == 0`), i.e. `glVertexAttribPointer` was given a pointer into CLIENT memory (the
/// weston-simple-egl / immediate-ish GL pattern). The deferred model can only read that client memory at
/// the moment the draw is recorded (it may change before swap), so the bytes are snapshotted then —
/// de-interleaved and TIGHTLY packed for the vertex range the draw touches — and lowered at swap into a
/// transient per-draw VERTEX buffer + a one-attribute vertex-layout slot (`CreateBuffer`/`WriteBuffer` +
/// `SetVertexBuffer`, the same path a real VBO uses).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ClientArray {
    /// The attribute location this array feeds — exactly the `layout(location = N)` the GLSL translator
    /// emits, so the lowered vertex-layout attribute's `location` matches the shader.
    pub location: usize,
    /// Tightly-packed captured bytes: one `size * component_size(kind)` element per touched vertex, with
    /// the client stride removed (element `v` of the range at byte `v * size * component_size`).
    pub data: Vec<u8>,
    /// Component count (1..4), component type, and the normalize/integer flags — the vertex-format the
    /// pipeline slot declares (mirrors the `Attr` fields, so `vertex_format_wire` produces the same code).
    pub size: i32,
    pub kind: u32,
    pub normalized: bool,
    pub integer: bool,
    /// The attribute's instance-step divisor, carried so the transient slot steps per-instance when set.
    pub divisor: u32,
}

/// One VBO/EBO generation captured when a draw is recorded. Deferred lowering happens at swap, after the
/// app may have orphaned or overwritten the same GL buffer name, so draw-time bytes are part of the draw.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BufferSnapshot {
    pub name: u32,
    pub generation: u64,
    pub data: Vec<u8>,
}

/// Identity and shape of an offscreen framebuffer attachment when a draw was recorded. Framebuffer
/// bindings are mutable GL state: Chrome can redefine an attachment between tile passes while retaining
/// the same GL texture name. The generation is therefore part of render-target identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetSnapshot {
    pub texture: u32,
    pub generation: u64,
    pub width: i32,
    pub height: i32,
    pub format: TextureFormat,
}

/// An immutable snapshot of the bound draw state at the moment a draw (or clear) is recorded. The frame
/// builder replays the draw-list into IR at swap. Ported from `hl-shim-gl`'s `DrawCall` (trimmed to the
/// fields the core single-draw / clear path uses this pass).
#[derive(Clone, PartialEq, Debug)]
pub struct DrawCall {
    /// A `glClear`-recorded rect rather than a geometry draw.
    pub is_clear: bool,
    /// GL primitive mode (`GL_TRIANGLES` / `GL_TRIANGLE_STRIP` / …).
    pub mode: u32,
    pub first: i32,
    pub count: i32,
    pub indexed: bool,
    pub index_type: u32,
    pub index_offset: usize,
    pub instance_count: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
    /// GL program name this draw renders with.
    pub prog: u32,
    /// The framebuffer bound when the draw was recorded (`0` = default window framebuffer; non-zero =
    /// render into that FBO's color attachment instead of the default surface).
    pub fbo: u32,
    /// Offscreen color attachment captured at draw time; `None` for the default framebuffer.
    pub target: Option<TargetSnapshot>,
    /// Bound element-array-buffer name (for an indexed draw).
    pub elem_buf: u32,
    /// Per-location vertex-attribute snapshot.
    pub attrs: [Attr; MAX_ATTR],
    /// Bound texture (GL name) per texture unit, at draw time.
    pub tex_units: [u32; 8],
    /// Content generation for each snapshotted texture-unit name.
    pub tex_generations: [u64; 8],
    /// The ES3 sampler OBJECT bound to each texture unit (`glBindSampler`), captured at draw time. A bound
    /// sampler object OVERRIDES the texture's own filter/wrap (ES 3.0 §3.8.13) — the frame builder lowers
    /// its params into the `SamplerDesc` instead of the texture's. `None` = no sampler object bound at the
    /// unit, so the texture parameters win (the byte-identical pre-sampler-object path).
    pub samp_objs: [Option<crate::model::es3::SamplerObj>; 8],
    /// Sampler-uniform index → texture unit, at draw time.
    pub samp_units: [i32; 4],
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],
    pub blend: bool,
    /// Blend factors/equations in force (GL enums), lowered to the pipeline blend state when `blend`.
    pub blend_src_rgb: u32,
    pub blend_dst_rgb: u32,
    pub blend_src_alpha: u32,
    pub blend_dst_alpha: u32,
    pub blend_eq_rgb: u32,
    pub blend_eq_alpha: u32,
    /// Constant blend color snapshotted at draw time (`glBlendColor`).
    pub blend_color: [f32; 4],
    pub depth: bool,
    /// Depth-compare function (GL enum) + depth-write mask, lowered to the pipeline depth state.
    pub depth_func: u32,
    pub depth_write: bool,
    /// `GL_STENCIL_TEST` enabled at draw time, and the front/back stencil test snapshot: per-face compare
    /// func + stencil-fail/depth-fail/depth-pass ops (GL enums), plus the front-face reference value and
    /// read/write masks (WebGPU carries a single reference + read/write mask for both faces). Lowered to the
    /// pipeline `DepthState` stencil fields + an `Enc::SetStencilReference`.
    pub stencil: bool,
    pub stencil_func_front: u32,
    pub stencil_func_back: u32,
    pub stencil_fail_front: u32,
    pub stencil_zfail_front: u32,
    pub stencil_zpass_front: u32,
    pub stencil_fail_back: u32,
    pub stencil_zfail_back: u32,
    pub stencil_zpass_back: u32,
    pub stencil_ref: i32,
    pub stencil_read_mask: u32,
    pub stencil_write_mask: u32,
    /// Face culling: whether `GL_CULL_FACE` is enabled, the culled face, and the front-face winding.
    pub cull_enabled: bool,
    pub cull_face: u32,
    pub front_face: u32,
    /// `glColorMask` per-channel write enable at draw time, packed `R<<0 | G<<1 | B<<2 | A<<3` — lowered
    /// verbatim into every color target's `ColorTargetState::write_mask`. `0xf` = write all channels.
    pub color_mask: u32,
    /// The clear color in force for this draw / clear.
    pub clear: [f32; 4],
    /// For a clear call: the (x, y, w, h) rect being cleared.
    pub clear_rect: [i32; 4],
    /// Captured client-side vertex arrays (enabled attribs drawn with NO VBO bound). EMPTY for an
    /// all-VBO draw — so a bound-VBO draw lowers byte-identically. Each entry lowers to a transient
    /// vertex buffer + a one-attribute vertex-layout slot (see [`ClientArray`]).
    pub client_vbufs: Vec<ClientArray>,
    /// Captured client-side INDEX bytes (`glDrawElements` with NO element-array-buffer bound: the index
    /// pointer is client memory). EMPTY otherwise. Already in the final index-buffer encoding — an
    /// unsigned-byte source is promoted to `u16` here (the index IR has no `u8` format), and `index_type`
    /// is rewritten to `GL_UNSIGNED_SHORT` to match. Lowered to a transient index buffer + `SetIndexBuffer`.
    pub client_indices: Vec<u8>,
    /// Bound vertex/index buffer generations used by this draw, captured before later GL mutations can
    /// change their contents. Chrome/Skia streams several batches through the same buffer name per frame.
    pub buffers: Vec<BufferSnapshot>,
    /// The std140 bytes of the app's uniform BLOCK for this draw, snapshotted at record time from the
    /// buffer bound via `glBindBufferBase(GL_UNIFORM_BUFFER, blockBinding, buffer)` to the program's block
    /// binding point (GskGpu/GTK4's per-frame `PushConstants { mat4 mvp; mat3x4 clip; vec2 scale; }`). The
    /// app already laid these out std140, so the frame builder binds them VERBATIM at IR binding 0 — this
    /// is what carries the real per-draw transform to the shader. EMPTY when the program feeds its uniforms
    /// the default-block `glUniform*` way (the ES2 `gl_multitex`/`gl_geometry` path), which stays on
    /// `Program::ubuf` unchanged. Snapshotted (not resolved at swap) because the app updates the UBO
    /// per-draw — the bytes must be captured at the draw they belong to, exactly like `client_vbufs`.
    pub ubo_bytes: Vec<u8>,
    /// The default-block `glUniform*` bytes (`Program::ubuf[..ubuf_size]`) snapshotted at record time.
    /// Like `ubo_bytes`, this is captured PER DRAW because `Program::ubuf` is mutable program state: a
    /// frame that draws the same program twice with different `glUniform*` values between the draws (e.g.
    /// a background color then an overlay color) would otherwise see every draw take the LAST-set values.
    /// EMPTY when the program feeds its uniforms via a `glBindBufferBase`d block (`ubo_bytes`) or has no
    /// default uniforms — the frame builder then falls back to `Program::ubuf` (byte-identical old path).
    pub ubuf_bytes: Vec<u8>,
}

impl Default for DrawCall {
    fn default() -> Self {
        Self {
            is_clear: false,
            mode: 0,
            first: 0,
            count: 0,
            indexed: false,
            index_type: 0,
            index_offset: 0,
            instance_count: 1,
            base_vertex: 0,
            first_instance: 0,
            prog: 0,
            fbo: 0,
            target: None,
            elem_buf: 0,
            attrs: [Attr::default(); MAX_ATTR],
            tex_units: [0; 8],
            tex_generations: [0; 8],
            samp_objs: [None; 8],
            samp_units: [-1; 4],
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            blend: false,
            blend_src_rgb: crate::model::glconst::GL_ONE,
            blend_dst_rgb: crate::model::glconst::GL_ZERO,
            blend_src_alpha: crate::model::glconst::GL_ONE,
            blend_dst_alpha: crate::model::glconst::GL_ZERO,
            blend_eq_rgb: crate::model::glconst::GL_FUNC_ADD,
            blend_eq_alpha: crate::model::glconst::GL_FUNC_ADD,
            blend_color: [0.0; 4],
            depth: false,
            depth_func: crate::model::glconst::GL_LESS,
            depth_write: true,
            stencil: false,
            stencil_func_front: crate::model::glconst::GL_ALWAYS,
            stencil_func_back: crate::model::glconst::GL_ALWAYS,
            stencil_fail_front: crate::model::glconst::GL_KEEP,
            stencil_zfail_front: crate::model::glconst::GL_KEEP,
            stencil_zpass_front: crate::model::glconst::GL_KEEP,
            stencil_fail_back: crate::model::glconst::GL_KEEP,
            stencil_zfail_back: crate::model::glconst::GL_KEEP,
            stencil_zpass_back: crate::model::glconst::GL_KEEP,
            stencil_ref: 0,
            stencil_read_mask: 0xffff_ffff,
            stencil_write_mask: 0xffff_ffff,
            cull_enabled: false,
            cull_face: crate::model::glconst::GL_BACK,
            front_face: crate::model::glconst::GL_CCW,
            color_mask: 0xf,
            clear: [0.0; 4],
            clear_rect: [0; 4],
            client_vbufs: Vec::new(),
            client_indices: Vec::new(),
            buffers: Vec::new(),
            ubo_bytes: Vec::new(),
            ubuf_bytes: Vec::new(),
        }
    }
}

/// The per-context shader + program tables: GL name → object, each with a monotonic name counter. GL
/// shares one name space per object kind; name `0` is the reserved sentinel.
#[derive(Debug, Default)]
pub struct Programs {
    shaders: HashMap<u32, Shader>,
    programs: HashMap<u32, Program>,
    next_shader: u32,
    next_program: u32,
}

impl Programs {
    pub fn new() -> Self {
        Self {
            shaders: HashMap::new(),
            programs: HashMap::new(),
            next_shader: 1,
            next_program: 1,
        }
    }

    /// `glCreateShader(kind)` — mint a shader name.
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        let name = self.next_shader;
        self.next_shader += 1;
        self.shaders.insert(
            name,
            Shader {
                kind,
                ..Default::default()
            },
        );
        name
    }

    /// `glShaderSource(shader, src)`.
    pub fn shader_source(&mut self, name: u32, src: &str) {
        if let Some(sh) = self.shaders.get_mut(&name) {
            sh.src = Some(src.to_string());
        }
    }

    /// `glCompileShader(shader)`.
    pub fn compile_shader(&mut self, name: u32) {
        if let Some(sh) = self.shaders.get_mut(&name) {
            sh.compiled = true;
        }
    }

    pub fn shader(&self, name: u32) -> Option<&Shader> {
        self.shaders.get(&name)
    }

    /// `glCreateProgram()` — mint a program name.
    pub fn create(&mut self) -> u32 {
        let name = self.next_program;
        self.next_program += 1;
        self.programs.insert(name, Program::default());
        name
    }

    /// `glAttachShader(program, shader)` — record the attachment by the shader's kind.
    pub fn attach(&mut self, program: u32, shader: u32) {
        let kind = self.shaders.get(&shader).map(|s| s.kind).unwrap_or(0);
        if let Some(p) = self.programs.get_mut(&program) {
            match kind {
                crate::model::glconst::GL_VERTEX_SHADER => p.vs = shader,
                crate::model::glconst::GL_FRAGMENT_SHADER => p.fs = shader,
                crate::model::glconst::GL_COMPUTE_SHADER => p.cs = shader,
                _ => {}
            }
        }
    }

    /// `glLinkProgram(program)` — translate + reflect the attached shaders. Returns `false` if the
    /// program name or either attached shader is unknown.
    pub fn link(&mut self, program: u32) -> bool {
        let (vs, fs, cs) = match self.programs.get(&program) {
            Some(p) => (p.vs, p.fs, p.cs),
            None => return false,
        };
        let vs_src = self
            .shaders
            .get(&vs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        let fs_src = self
            .shaders
            .get(&fs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        let cs_src = self
            .shaders
            .get(&cs)
            .and_then(|s| s.src.clone())
            .unwrap_or_default();
        if let Some(p) = self.programs.get_mut(&program) {
            p.link(vs_src, fs_src, cs_src);
            true
        } else {
            false
        }
    }

    pub fn program(&self, name: u32) -> Option<&Program> {
        self.programs.get(&name)
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut Program> {
        self.programs.get_mut(&name)
    }

    /// `glIsProgram(name)` — true once `name` names a live program object (`0` is never a program).
    pub fn contains(&self, name: u32) -> bool {
        name != 0 && self.programs.contains_key(&name)
    }

    /// `glIsShader(name)` — true once `name` names a live shader object.
    pub fn shader_exists(&self, name: u32) -> bool {
        name != 0 && self.shaders.contains_key(&name)
    }

    /// `glDeleteProgram(name)` — drop the program object (deleting `0` is a silent no-op; GL defines it
    /// so). This model has no deferred-delete-while-current subtlety: the object is removed immediately.
    /// Returns `false` for an unknown / zero name.
    pub fn delete(&mut self, name: u32) -> bool {
        if name == 0 {
            return false;
        }
        self.programs.remove(&name).is_some()
    }

    /// `glDeleteShader(name)` — drop the shader object (deleting `0` is a silent no-op). Attachments hold
    /// only the shader NAME (captured at link), so a delete does not disturb a linked program's reflected
    /// IR. Returns `false` for an unknown / zero name.
    pub fn delete_shader(&mut self, name: u32) -> bool {
        if name == 0 {
            return false;
        }
        self.shaders.remove(&name).is_some()
    }

    /// `glDetachShader(program, shader)` — clear the matching attachment slot. Returns `false` if the
    /// program is unknown or `shader` is not attached (the caller raises the spec error).
    pub fn detach(&mut self, program: u32, shader: u32) -> bool {
        match self.programs.get_mut(&program) {
            Some(p) if p.vs == shader => {
                p.vs = 0;
                true
            }
            Some(p) if p.fs == shader => {
                p.fs = 0;
                true
            }
            Some(p) if p.cs == shader => {
                p.cs = 0;
                true
            }
            _ => false,
        }
    }
}
