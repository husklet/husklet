//! Program link translation and reflected resource initialization.

use super::Program;
use crate::adapter::glsl;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_LINK_DIAGNOSTICS: usize = 128;
static LINK_DIAGNOSTICS: AtomicUsize = AtomicUsize::new(0);
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

const SHADER_DIAGNOSTIC_LIMIT: usize = 64;
const SHADER_SOURCE_DIAGNOSTIC_BYTES: usize = 4096;

fn without_static_false_blocks(source: &str) -> String {
    let mut output = source.to_string();
    for condition in ["if (0 != 0)", "if(0 != 0)", "if (0!=0)", "if(0!=0)"] {
        while let Some(start) = output.find(condition) {
            let Some(open) = output[start + condition.len()..].find('{').map(|at| start + condition.len() + at)
            else {
                break;
            };
            let mut depth = 0usize;
            let mut end = None;
            for (offset, byte) in output.as_bytes()[open..].iter().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = Some(open + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { break };
            output.replace_range(start..end, "");
        }
    }
    output
}

fn identifier_occurrences(source: &str, name: &str) -> usize {
    source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|word| *word == name)
        .count()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ShaderKey {
    vertex: u64,
    fragment: u64,
}

struct ShaderDiagnostics;

impl ShaderDiagnostics {
    fn hash(source: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        hasher.finish()
    }

    fn prefix(source: &str) -> &str {
        let mut end = source.len().min(SHADER_SOURCE_DIAGNOSTIC_BYTES);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        &source[..end]
    }

    fn log(verbatim: bool, vertex: &str, fragment: &str, samplers: &[String], bindings: &[u32]) {
        let logging = hl_log::Logging::global();
        let tags = hl_log::Tags::from(hl_log::tag::GL);
        let debug = hl_log::VERBOSE_COMPILED && logging.enabled(tags, hl_log::Level::Debug);
        let trace = hl_log::VERBOSE_COMPILED && logging.enabled(tags, hl_log::Level::Trace);
        if !debug && !trace {
            return;
        }

        static SHADERS: OnceLock<Mutex<HashSet<ShaderKey>>> = OnceLock::new();
        let key = ShaderKey {
            vertex: Self::hash(vertex),
            fragment: Self::hash(fragment),
        };
        let shaders = SHADERS.get_or_init(|| Mutex::new(HashSet::new()));
        let Ok(mut shaders) = shaders.lock() else {
            return;
        };
        if shaders.len() >= SHADER_DIAGNOSTIC_LIMIT || !shaders.insert(key) {
            return;
        }
        drop(shaders);

        hl_log::hl_debug!(
            hl_log::tag::GL,
            "shader_link key={:016x}:{:016x} verbatim={} vertex_bytes={} fragment_bytes={} samplers={:?} bindings={:?}",
            key.vertex,
            key.fragment,
            verbatim,
            vertex.len(),
            fragment.len(),
            samplers,
            bindings
        );
        hl_log::hl_trace!(
            hl_log::tag::GL,
            "shader_source key={:016x}:{:016x} vertex_bytes={} fragment_bytes={}\n--- vertex (first {} bytes) ---\n{}\n--- fragment (first {} bytes) ---\n{}\n--- end shader ---",
            key.vertex,
            key.fragment,
            vertex.len(),
            fragment.len(),
            Self::prefix(vertex).len(),
            Self::prefix(vertex),
            Self::prefix(fragment).len(),
            Self::prefix(fragment)
        );
    }
}

impl Program {
    /// `glLinkProgram` — translate the attached shaders to shader-IR and reflect the layout.
    ///
    /// A **render** program (a vertex+fragment pair) translates the GLSL-ES pair to combined MSL
    /// (`shader_ir`) and reflects the uniform-block + sampler layout. A **compute** program (`cs_src`
    /// non-empty) translates the compute source to `compute_ir`, which `glDispatchCompute` lowers to a
    /// `CreateShader` + `CreateComputePipeline`.
    pub fn link(
        &mut self,
        vs_src: String,
        fs_src: String,
        cs_src: String,
    ) -> Result<(), glsl::UniformError> {
        use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
        // A (re)link produces fresh shader IR + reflection; bump the generation so the frame builder's
        // program-keyed shader/pipeline cache invalidates any IR it created for the previous link.
        self.link_gen += 1;
        if !cs_src.is_empty() {
            // The compute path carries no default uniform block (see `MAX_COMPUTE_UNIFORM_COMPONENTS`):
            // refuse such a program instead of letting every `glUniform*` on it read back zero.
            if let Some(name) = glsl::compute_default_block_uniform(&cs_src)? {
                return Err(glsl::UniformError::ComputeDefaultBlock(name));
            }
            if !glsl::Source::new(&cs_src).has_main_body() {
                return Err(glsl::UniformError::MainBody { stage: "compute" });
            }
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
            self.link_error.clear();
            return Ok(());
        }
        // A stage the translator cannot find a complete `main` in must be refused BEFORE anything is
        // regenerated from it. `translate_render` rebuilds a stage from its reflected declarations plus
        // its main body; given a shader with a dropped closing brace it used to emit `void main() {}`,
        // which the host front end correctly accepts. The frame then drew nothing, wrote no pixel, and
        // reported success at every layer — the failure presents as a blank, fully transparent region
        // rather than as an error. Measured over the corpus: every one of 42 real shaders with a single
        // closing brace removed travelled the whole chain and rendered nothing.
        for (stage, source) in [("vertex", &vs_src), ("fragment", &fs_src)] {
            if !glsl::Source::new(source).has_main_body() {
                return Err(glsl::UniformError::MainBody { stage });
            }
        }
        glsl::StageSources::new(&vs_src, &fs_src).validate_sampler_array_uses()?;
        let (unis, ubuf_size) = glsl::StageSources::new(&vs_src, &fs_src).uniform_layout()?;
        let sampler_decls = glsl::StageSources::new(&vs_src, &fs_src).sampler_decls();
        self.samp_names = sampler_decls
            .iter()
            .map(|declaration| declaration.name.clone())
            .collect();
        self.samp_types = sampler_decls
            .iter()
            .map(|declaration| declaration.ty.clone())
            .collect();
        self.samp_arrays = sampler_decls
            .iter()
            .map(|declaration| declaration.arr.max(1))
            .collect();
        let sampler_elements = self
            .samp_arrays
            .iter()
            .map(|&elements| elements as usize)
            .sum::<usize>();
        // GLES sampler uniforms are integer uniforms and therefore start at zero. Multiple untouched
        // samplers consequently all refer to texture unit 0 until the application assigns another unit.
        self.samp_units = vec![0; sampler_elements];
        // GskGpu (GTK4 "gl") / ANGLE (Chrome) source uses helper functions taking combined sampler
        // parameters and `gl_VertexID` vertex-pulling — constructs `translate_render`'s reflect-and-
        // regenerate would destroy (it keeps only `main` and reflects a flat ES2 interface). For such
        // source, forward the stage VERBATIM so the host executor's ES route (`glsl_es` +
        // `spirv_split`-style combined→separate sampler split) compiles the REAL text. The sampler /
        // uniform reflection above still drives the bind-group layout, which stays on the shared
        // `binding = 1+2k / 2+2k` scheme the executor emits. Simple ES2 shaders take `translate_render`.
        let verbatim = glsl::Source::new(&vs_src).is_forward_verbatim()
            || glsl::Source::new(&fs_src).is_forward_verbatim();
        let active_attributes = glsl::Source::new(&vs_src)
            .vertex_attrs()
            .into_iter()
            .map(|attribute| attribute.name)
            .collect::<std::collections::BTreeSet<_>>();
        let mut attribute_bindings = self.attrib_bindings.clone();
        attribute_bindings.retain(|name, _| active_attributes.contains(name));
        // A shader's explicit location overrides a pre-link API binding.
        attribute_bindings.extend(glsl::Source::new(&vs_src).vertex_locations());
        let declarations = glsl::Source::new(&vs_src).vertex_attrs();
        for (name, &location) in &attribute_bindings {
            let span = declarations
                .iter()
                .find(|declaration| declaration.name == *name)
                .map_or(1, |declaration| declaration.location_span());
            if location as usize >= super::MAX_ATTR
                || location.saturating_add(span) as usize > super::MAX_ATTR
            {
                return Err(glsl::UniformError::AttributeLocation(name.clone()));
            }
        }
        let mut public_attributes = if verbatim {
            // Verbatim preparation performs the same span-aware allocation for unbound names.
            let combined = glsl::StageSources::new(&vs_src, &fs_src).storage_uniform_decls();
            let (vertex, _) = glsl::prepare_verbatim_program_with(
                &vs_src,
                &fs_src,
                &combined,
                &attribute_bindings,
            );
            glsl::Source::new(&vertex).vertex_locations()
        } else {
            let (vertex, _) = glsl::StageSources::new(&vs_src, &fs_src)
                .translate_render_with(&attribute_bindings);
            glsl::Source::new(&vertex).vertex_locations()
        };
        // Translation canonicalizes equivalent aliased inputs into one host declaration. Every active GL
        // name still reflects the location explicitly assigned through glBindAttribLocation.
        for declaration in &declarations {
            if let Some(&location) = attribute_bindings.get(&declaration.name) {
                public_attributes.insert(declaration.name.clone(), location);
            }
        }
        // The verbatim route preserves helper functions, but its prepared-source location scan can omit
        // legacy ES `attribute PRECISION TYPE name` declarations. They are still active GL attributes and
        // need an automatically allocated public location just like the regenerated ES2 route provides.
        for declaration in &declarations {
            if public_attributes.contains_key(&declaration.name) {
                continue;
            }
            let span = declaration.location_span() as usize;
            let location = (0..=super::MAX_ATTR.saturating_sub(span))
                .find(|&candidate| {
                    public_attributes.iter().all(|(name, &base)| {
                        let occupied_span = declarations
                            .iter()
                            .find(|other| other.name == *name)
                            .map_or(1, |other| other.location_span())
                            as usize;
                        candidate + span <= base as usize
                            || candidate >= base as usize + occupied_span
                    })
                })
                .ok_or_else(|| glsl::UniformError::AttributeLocation(declaration.name.clone()))?;
            public_attributes.insert(declaration.name.clone(), location as u32);
        }
        // GL permits names to alias one public attribute location. WebGPU does not permit duplicate
        // shader locations, so give every declaration a collision-free host-only range. Draw lowering
        // duplicates the public GL array into each host range that consumes it.
        let mut host_bindings = std::collections::BTreeMap::new();
        let mut host_occupied = [false; super::MAX_ATTR];
        for declaration in &declarations {
            let Some(&public) = public_attributes.get(&declaration.name) else {
                continue;
            };
            if let Some(canonical_host) = declarations.iter().find_map(|candidate| {
                (candidate.name != declaration.name
                    && candidate.ty == declaration.ty
                    && candidate.arr == declaration.arr
                    && public_attributes.get(&candidate.name) == Some(&public))
                    .then(|| host_bindings.get(&candidate.name).copied())
                    .flatten()
            }) {
                host_bindings.insert(declaration.name.clone(), canonical_host);
                continue;
            }
            let span = declaration.location_span() as usize;
            let preferred = public as usize;
            let host = (0..=super::MAX_ATTR.saturating_sub(span))
                .find(|&base| {
                    let preferred_free = base == preferred
                        && host_occupied[base..base + span].iter().all(|used| !used);
                    preferred_free
                })
                .or_else(|| {
                    (0..=super::MAX_ATTR.saturating_sub(span))
                        .find(|&base| host_occupied[base..base + span].iter().all(|used| !used))
                })
                .ok_or_else(|| glsl::UniformError::AttributeLocation(declaration.name.clone()))?;
            host_occupied[host..host + span].fill(true);
            host_bindings.insert(declaration.name.clone(), host as u32);
        }
        // Link reflection excludes inputs referenced only from a compile-time-false block. Keep their
        // host alias while translating—the dead expression remains syntactically present—but do not expose
        // them through GL_ACTIVE_ATTRIBUTES / glGetAttribLocation.
        let live_vertex = without_static_false_blocks(&vs_src);
        public_attributes.retain(|name, _| identifier_occurrences(&live_vertex, name) > 1);
        if !attribute_bindings.is_empty() {
            let sample = attribute_bindings.iter().take(8).collect::<Vec<_>>();
            hl_log::hl_debug!(
                hl_log::tag::GL,
                "link attribute bindings active={} sample={sample:?}",
                attribute_bindings.len()
            );
        }
        let (mut vs_glsl, mut fs_glsl) = if verbatim {
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
            let combined = glsl::StageSources::new(&vs_src, &fs_src).storage_uniform_decls();
            glsl::prepare_verbatim_program_with(&vs_src, &fs_src, &combined, &host_bindings)
        } else {
            glsl::StageSources::new(&vs_src, &fs_src).translate_render_with(&host_bindings)
        };
        if verbatim {
            vs_glsl = glsl::Source::new(&vs_glsl).expand_sampler_arrays();
            fs_glsl = glsl::Source::new(&fs_glsl).expand_sampler_arrays();
        }
        // Bind-group binding index per sampler. The ES2 path EMITS its own `layout(binding=)` in declaration
        // order, so `k == index`; the verbatim path is numbered by the HOST across all preprocessor branches
        // (incl. inactive `samplerExternalOES` decls), so `k` is recovered from the forwarded fragment text.
        self.samp_bindings = if verbatim {
            let flattened_names = self
                .samp_names
                .iter()
                .zip(&self.samp_arrays)
                .flat_map(|(name, &elements)| {
                    (0..elements).map(move |element| {
                        if elements == 1 {
                            name.clone()
                        } else {
                            format!("{name}_{element}")
                        }
                    })
                })
                .collect::<Vec<_>>();
            glsl::StageSources::new("", &fs_glsl).verbatim_sampler_bindings(&flattened_names)
        } else {
            (0..sampler_elements as u32).collect()
        };
        ShaderDiagnostics::log(
            verbatim,
            &vs_glsl,
            &fs_glsl,
            &self.samp_names,
            &self.samp_bindings,
        );
        self.attrib_locations = public_attributes;
        self.attrib_host_locations = host_bindings;
        self.vs_ir = Some(
            GlslDescriptor {
                stage: glsl_stage::VERTEX,
                entry: "vmain".into(),
                source: vs_glsl.clone(),
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
        if self.transform_feedback_names.is_empty() {
            self.transform_feedback_layout = None;
            self.transform_feedback_ir = None;
        } else {
            let layout = super::TransformFeedbackLayout::reflect(
                &vs_src,
                &self.transform_feedback_names,
                self.transform_feedback_mode,
            )
            .map_err(glsl::UniformError::TransformFeedback)?;
            let capture_source = layout
                .capture_source(&vs_glsl)
                .map_err(glsl::UniformError::TransformFeedback)?;
            self.transform_feedback_ir = Some(
                GlslDescriptor {
                    stage: glsl_stage::VERTEX,
                    entry: "vmain".into(),
                    source: capture_source,
                }
                .to_words(),
            );
            self.transform_feedback_layout = Some(layout);
        }
        self.unis = unis;
        self.ubuf_size = ubuf_size;
        self.ubuf = vec![0u8; ubuf_size.max(0) as usize];
        if LINK_DIAGNOSTICS.fetch_add(1, Ordering::Relaxed) < MAX_LINK_DIAGNOSTICS {
            let fields = self
                .unis
                .iter()
                .map(|uniform| {
                    format!(
                        "{}:{}[{}]@{}+{}",
                        uniform.name,
                        uniform.ty,
                        uniform.arr.max(1),
                        uniform.off,
                        uniform.sz
                    )
                })
                .collect::<Vec<_>>();
            hl_log::hl_debug!(
                hl_log::tag::GL,
                "uniform_link verbatim={verbatim} bytes={} fields={fields:?} samplers={:?} bindings={:?}",
                self.ubuf_size,
                self.samp_names,
                self.samp_bindings
            );
        }
        self.vs_src = vs_src;
        self.fs_src = fs_src;
        self.linked = true;
        self.link_error.clear();
        Ok(())
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
