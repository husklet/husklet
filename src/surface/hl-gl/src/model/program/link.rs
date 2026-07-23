//! Program link translation and reflected resource initialization.

use super::Program;
use crate::adapter::glsl;

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
}
