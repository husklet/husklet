//! GL shader + program objects, the shader/program table, and the recorded-draw types.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Shader`, `Program`, `Attr`, `DrawCall`). A shader keeps its
//! GLSL-ES source; a program keeps its attached vertex+fragment sources and, once linked, the
//! translated shader-IR ([`adapter::glsl`](crate::adapter::glsl)) + uniform-block layout + sampler
//! bindings. GL is deferred-lowering, so a [`DrawCall`] is the immutable snapshot of the bound draw
//! state at the moment `glDrawArrays`/`glDrawElements` records it; the frame builder replays the
//! draw-list into IR at swap.

use crate::adapter::glsl::{self, Uni};
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
        if !cs_src.is_empty() {
            let source = glsl::translate_compute(&cs_src);
            self.compute_ir = Some(
                GlslDescriptor { stage: glsl_stage::COMPUTE, entry: "cmain".into(), source }.to_words(),
            );
            self.cs_src = cs_src;
            self.linked = true;
            return;
        }
        let (unis, ubuf_size) = glsl::uni_layout(&vs_src, &fs_src);
        self.samp_names = glsl::program_samplers(&vs_src, &fs_src);
        let (vs_glsl, fs_glsl) = glsl::translate_render(&vs_src, &fs_src);
        self.vs_ir = Some(
            GlslDescriptor { stage: glsl_stage::VERTEX, entry: "vmain".into(), source: vs_glsl }
                .to_words(),
        );
        self.fs_ir = Some(
            GlslDescriptor { stage: glsl_stage::FRAGMENT, entry: "fmain".into(), source: fs_glsl }
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
        crate::adapter::glsl::collect_vertex_attrs(&self.vs_src)
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
    /// Bound element-array-buffer name (for an indexed draw).
    pub elem_buf: u32,
    /// Per-location vertex-attribute snapshot.
    pub attrs: [Attr; MAX_ATTR],
    /// Bound texture (GL name) per texture unit, at draw time.
    pub tex_units: [u32; 8],
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
    pub depth: bool,
    /// Depth-compare function (GL enum) + depth-write mask, lowered to the pipeline depth state.
    pub depth_func: u32,
    pub depth_write: bool,
    /// Face culling: whether `GL_CULL_FACE` is enabled, the culled face, and the front-face winding.
    pub cull_enabled: bool,
    pub cull_face: u32,
    pub front_face: u32,
    /// The clear color in force for this draw / clear.
    pub clear: [f32; 4],
    /// For a clear call: the (x, y, w, h) rect being cleared.
    pub clear_rect: [i32; 4],
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
            elem_buf: 0,
            attrs: [Attr::default(); MAX_ATTR],
            tex_units: [0; 8],
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
            depth: false,
            depth_func: crate::model::glconst::GL_LESS,
            depth_write: true,
            cull_enabled: false,
            cull_face: crate::model::glconst::GL_BACK,
            front_face: crate::model::glconst::GL_CCW,
            clear: [0.0; 4],
            clear_rect: [0; 4],
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
        Self { shaders: HashMap::new(), programs: HashMap::new(), next_shader: 1, next_program: 1 }
    }

    /// `glCreateShader(kind)` — mint a shader name.
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        let name = self.next_shader;
        self.next_shader += 1;
        self.shaders.insert(name, Shader { kind, ..Default::default() });
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
    pub fn create_program(&mut self) -> u32 {
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
        let vs_src = self.shaders.get(&vs).and_then(|s| s.src.clone()).unwrap_or_default();
        let fs_src = self.shaders.get(&fs).and_then(|s| s.src.clone()).unwrap_or_default();
        let cs_src = self.shaders.get(&cs).and_then(|s| s.src.clone()).unwrap_or_default();
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

    pub fn program_mut(&mut self, name: u32) -> Option<&mut Program> {
        self.programs.get_mut(&name)
    }

    /// `glIsProgram(name)` — true once `name` names a live program object (`0` is never a program).
    pub fn program_exists(&self, name: u32) -> bool {
        name != 0 && self.programs.contains_key(&name)
    }

    /// `glIsShader(name)` — true once `name` names a live shader object.
    pub fn shader_exists(&self, name: u32) -> bool {
        name != 0 && self.shaders.contains_key(&name)
    }

    /// `glDeleteProgram(name)` — drop the program object (deleting `0` is a silent no-op; GL defines it
    /// so). This model has no deferred-delete-while-current subtlety: the object is removed immediately.
    /// Returns `false` for an unknown / zero name.
    pub fn delete_program(&mut self, name: u32) -> bool {
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
