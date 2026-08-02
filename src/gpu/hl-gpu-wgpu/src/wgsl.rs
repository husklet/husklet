//! Host-side shader translation to WGSL — the single seam that turns every shader payload the protocol
//! carries into the one language wgpu compiles.
//!
//! Two source languages feed it:
//!
//! * **kernel-IR** ([`KernelProgram`]): [`kernel_to_wgsl`] lowers the neutral compute IR (the compiled
//!   form of a driver's PTX front-end) to a WGSL compute entry point. The CPU oracle *interprets* this IR;
//!   here it becomes real WGSL that runs on the GPU. Ported verbatim (semantics-preserving) from
//!   `hl-gpu/src/ptx.rs::kernel_to_wgsl` so the executed kernel matches the oracle byte-for-byte.
//! * **SPIR-V / GLSL** graphics shaders: [`spirv_to_wgsl`] / [`glsl_to_wgsl`] run naga (spv-in / glsl-in →
//!   wgsl-out). Ported from the reference `hl-gpu-wgpu/src/shader.rs`.
//!
//! The kernel ABI the emitted compute WGSL declares: `@group(0) @binding(0)` is the flat `params` blob
//! (`array<u32>`, read), and `@binding(r+1)` is pointer region `r` (`array<u32>`/`array<atomic<u32>>`,
//! read_write) for `r in 0..num_regions`. A bind group built from the protocol descriptor maps binding 0
//! → the param buffer and binding `r+1` → the region-`r` storage buffer, exactly the layout the vecadd /
//! store-one conformance programs encode.

use hl_gpu::protocol::model::kernel::{
    gty, mem_scope, Inst, KernelProgram, Op, ATOM_ADD, ATOM_AND, ATOM_CAS, ATOM_EXCH, ATOM_MAX,
    ATOM_MIN, ATOM_OR, ATOM_XOR, BIT_AND, BIT_OR, CMP_EQ, CMP_GT, CMP_LE, CMP_LT, CMP_NE,
    CVT_F32_FROM_S32, CVT_F32_FROM_U32, CVT_IDENTITY, CVT_S32_FROM_F32, CVT_S32_FROM_F32_RNI,
    CVT_S64_FROM_S32, CVT_U32_FROM_F32, CVT_U32_FROM_F32_RNI, SHIFT_LEFT, SR_CTAID_X, SR_CTAID_Y,
    SR_CTAID_Z, SR_NCTAID_X, SR_NCTAID_Y, SR_NTID_X, SR_NTID_Y, SR_NTID_Z, SR_TID_X, SR_TID_Y,
    SR_TID_Z,
};
use hl_gpu::{GpuError, Result};

mod descriptor;
mod diagnostic;
mod module;
mod nonfinite;
mod texel_buffer;

use diagnostic::Diagnostic;
use module::ShaderModule;
pub(crate) mod viewport;

// ===================================================================================================
// SPIR-V / GLSL graphics shaders → WGSL (naga)
// ===================================================================================================

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Translate a SPIR-V word payload to WGSL via naga (spv-in → validate → wgsl-out). Returns the WGSL
/// text wgpu compiles. A payload without the SPIR-V magic is rejected (the strict ABI never falls back
/// to a built-in shader).
#[cfg(test)]
pub fn spirv_to_wgsl(words: &[u32]) -> Result<String> {
    Ok(Spirv::translate_reflect(words)?.0)
}

/// [`spirv_to_wgsl`] plus the module's DECLARED resource bindings ([`crate::reflect`]), reflected from the
/// naga module before `wgsl-out` so the pipeline can build an explicit bind-group layout matching the
/// driver's per-declared-resource bind group. See [`glsl_to_wgsl_reflect`].
pub struct Spirv;

impl Spirv {
    pub fn translate_reflect(words: &[u32]) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(words, None, false, None)
    }

    pub fn translate_reflect_layout(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(words, layout, false, None)
    }

    pub fn translate_reflect_sample_shading(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(words, layout, true, None)
    }

    pub(crate) fn translate_reflect_texel(
        words: &[u32],
        layout: &hl_gpu::protocol::model::descriptor::PipelineLayout,
        specialization: &[crate::texel_buffer::Specialization],
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_texel_sample(words, layout, specialization, false)
    }

    pub(crate) fn translate_reflect_texel_sample(
        words: &[u32],
        layout: &hl_gpu::protocol::model::descriptor::PipelineLayout,
        specialization: &[crate::texel_buffer::Specialization],
        sample_shading: bool,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(
            words,
            Some(layout),
            sample_shading,
            Some(specialization),
        )
    }

    fn translate_reflect_options(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
        sample_shading: bool,
        texel_specialization: Option<&[crate::texel_buffer::Specialization]>,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        if words.first().copied() != Some(SPIRV_MAGIC) {
            return Err(GpuError::Invalid("wgpu: shader payload is not SPIR-V"));
        }
        // glslang emits GLSL `sampler2D` as a COMBINED image-sampler (an `OpTypeSampledImage` global sampled
        // with no `OpSampledImage`), which naga's spv-in rejects. Rewrite it to the SEPARATE image+sampler
        // model naga accepts before parsing (a shader without a combined sampler passes through unchanged).
        let split = crate::spirv_split::CombinedSamplers::split(words)?;
        let bytes: &[u8] = bytemuck::cast_slice(&split);
        let mut module =
            naga::front::spv::parse_u8_slice(bytes, &naga::front::spv::Options::default())
                .map_err(|e| Diagnostic::kernel(format!("spirv-in: {e:?}")))?;
        // WGSL offers read-only and read_write storage buffers, but no write-only address space. SPIR-V
        // legitimately uses NonReadable on output SSBOs (including Vulkan CTS buffer-view results), which
        // naga preserves as STORE-only and then rejects at WGSL validation. Granting LOAD in the host type
        // does not add a shader read; it only selects WGSL's representable read_write spelling.
        for (_, variable) in module.global_variables.iter_mut() {
            if let naga::AddressSpace::Storage { access } = &mut variable.space {
                if access.contains(naga::StorageAccess::STORE) {
                    access.insert(naga::StorageAccess::LOAD);
                }
            }
        }
        if let Some(layout) = layout {
            for (_, variable) in module.global_variables.iter_mut() {
                let Some(resource) = &variable.binding else {
                    continue;
                };
                let Some(binding) = layout.bindings.iter().find(|binding| {
                    binding.group == resource.group
                        && binding.binding == resource.binding
                        && binding.count > 1
                }) else {
                    continue;
                };
                let expected = naga::ArraySize::Constant(
                    std::num::NonZeroU32::new(binding.count)
                        .ok_or(GpuError::Invalid("zero descriptor count"))?,
                );
                let _base =
                    match module.types[variable.ty].inner {
                        naga::TypeInner::Array { base, size, .. } => {
                            if size != expected {
                                return Err(GpuError::Invalid(
                                    "SPIR-V descriptor array count differs from pipeline layout",
                                ));
                            }
                            variable.ty = module.types.insert(
                                naga::Type {
                                    name: None,
                                    inner: naga::TypeInner::BindingArray { base, size },
                                },
                                naga::Span::default(),
                            );
                            base
                        }
                        naga::TypeInner::BindingArray { base, size } => {
                            if size != expected {
                                return Err(GpuError::Invalid(
                                    "SPIR-V descriptor array count differs from pipeline layout",
                                ));
                            }
                            base
                        }
                        // A layout that declares a descriptor ARRAY at this slot while the shader declares a
                        // plain resource is the same shader/layout disagreement the two arms above refuse;
                        // skipping it silently left the mismatch to be discovered, or not, downstream.
                        _ => return Err(GpuError::Invalid(
                            "SPIR-V global is not an array where the pipeline layout declares one",
                        )),
                    };
            }
        }
        if sample_shading {
            let sample_index = module.types.insert(
                naga::Type {
                    name: None,
                    inner: naga::TypeInner::Scalar(naga::Scalar {
                        kind: naga::ScalarKind::Uint,
                        width: 4,
                    }),
                },
                naga::Span::default(),
            );
            for entry in &mut module.entry_points {
                if entry.stage == naga::ShaderStage::Fragment
                    && !entry.function.arguments.iter().any(|argument| {
                        argument.binding == Some(naga::Binding::BuiltIn(naga::BuiltIn::SampleIndex))
                    })
                {
                    entry.function.arguments.push(naga::FunctionArgument {
                        name: Some("_hl_sample_index".into()),
                        ty: sample_index,
                        binding: Some(naga::Binding::BuiltIn(naga::BuiltIn::SampleIndex)),
                    });
                }
            }
        }
        if let Some(layout) = layout {
            descriptor::ScalarArrays::lower(&mut module, layout)?;
        }
        texel_buffer::TexelBuffers::lower(&mut module, texel_specialization)?;
        viewport::Shader::prepare(&mut module)?;
        let reflected = crate::reflect::ModuleUsage::from_module(&module);
        // `OpIsInf`/`OpIsNan` survive `spv-in` as relational expressions naga's `wgsl-out` cannot emit.
        // The GLSL route rewrites those in the source text, which a SPIR-V payload never has, so the
        // lowering has to happen on the IR both front ends share.
        let mut shader = ShaderModule::new(&mut module);
        shader.lower_nonfinite_predicates();
        Ok((shader.wgsl()?, reflected))
    }
}

/// Translate GLSL source (the forwarded GLES/GL driver path) to WGSL for `stage`, naming the emitted entry
/// point `entry`. naga's `glsl-in` always names the single entry point `main`; the render/compute pipeline
/// binds the driver-declared name (`vmain`/`fmain`/`cmain`) via its `ShaderRef`, so we rename the entry
/// point to `entry` before `wgsl-out` writes it. Handles vertex, fragment, and compute stages.
#[cfg(test)]
pub fn glsl_to_wgsl(src: &str, stage: naga::ShaderStage, entry: &str) -> Result<String> {
    Ok(glsl_to_wgsl_reflect(src, stage, entry)?.0)
}

/// [`glsl_to_wgsl`] plus the module's DECLARED resource bindings ([`crate::reflect`]). naga's `glsl-in`
/// keeps every declared `layout(binding=)` global in the module even when `main()` never reads it, so this
/// recovers the full set the GL driver bound a bind-group entry for — the datum
/// `pipeline::create_render_pipeline` builds its explicit bind-group layout from.
pub fn glsl_to_wgsl_reflect(
    src: &str,
    stage: naga::ShaderStage,
    entry: &str,
) -> Result<(String, crate::reflect::ModuleUsage)> {
    let original = src;
    // GskGpu (GTK4 "gl") and ANGLE (Chrome) emit GLSL-ES that naga's `glsl-in` rejects wholesale
    // (`#version … es`, `gl_VertexID`, combined `sampler2D` globals AND — the hard case — combined
    // `sampler2D` FUNCTION PARAMETERS). The host glslang/shaderc route that normally handles these is not
    // buildable offline, so [`crate::glsl_es`] performs the naga-relevant lowering (ES→460, vertex-index
    // builtins, and a combined→separate sampler split that crosses helper signatures) in pure Rust before
    // naga parses. Simple ES2 conformance shaders the GL driver already rewrote to desktop form are NOT
    // ES-shaped, so they keep the direct path below with zero change.
    let normalized;
    let src = if crate::glsl_es::Source::new(src).is_es() {
        hl_log::hl_debug!(
            hl_log::tag::WGPU,
            "glsl_to_wgsl: GLSL-ES/GskGpu source → es-normalize+sampler-split"
        );
        normalized = crate::glsl_es::Source::new(src).normalize(stage);
        normalized.as_str()
    } else {
        src
    };
    // naga's `glsl-in` rejects a 2-row matrix (`mat2`/`mat3x2`/`mat4x2`) in a std140 uniform block. That is
    // a layout restriction, not a dialect one, so the column-splitting workaround runs on BOTH routes — the
    // ES route above already applied it inside `normalize`, and this reaches the DESKTOP route, which is
    // where the GL driver's ES2 output lands after it rewrites its own shaders (`is_es()` is false for it).
    // Byte-faithful for any shader without such a member, so the direct path stays unchanged.
    // Two more passes that encode TARGET-language rules rather than dialect ones, and so must reach the
    // desktop route the GL driver emits. `index =` is a qualifier naga's `glsl-in` cannot parse in any
    // dialect; a fall-through switch case is something its `wgsl-out` cannot emit in any dialect. Both
    // previously ran only inside `normalize`, so the identical shader compiled as ES and was refused as
    // desktop. Both are byte-faithful when their construct is absent and idempotent after `normalize`.
    let point_size = crate::glsl_es::Source::new(src).normalize_fixed_point_size(stage);
    let dual = crate::glsl_es::Source::new(&point_size).normalize_dual_source();
    let lowered = crate::glsl_es::Source::new(&dual).lower_switch();
    let src = lowered.as_str();
    // A matrix cannot be a shader input or output in WGSL at all, so every matrix varying has to be split
    // into per-location vector slots. That pass lived only inside `normalize`, i.e. only on the ES route,
    // while the GL driver rewrites its shaders to desktop form before they arrive — so a plain matrix
    // varying was split for an ES guest and refused for the driver's own output. Layout and interface
    // rules are dialect-independent; this runs on both routes, and is idempotent after `normalize`.
    let split_io = crate::glsl_es::Source::new(src).split_aggregate_io(stage);
    let src = split_io.as_str();
    // The uniform address space requires a 16-byte array stride in WGSL, which is also what std140 mandates
    // and what the GL driver's own writes use — but naga's `glsl-in` gives `float u[4]` the element type's
    // natural stride (4), so the emitted module is refused by wgpu's validator. Padding those members to
    // arrays of `vec4` is the same kind of layout-only, dialect-independent rewrite, so it runs on BOTH
    // routes beside the 2-row-matrix split and is byte-faithful for a shader without such a member.
    let unmat2;
    let padded;
    let src = if src.contains("std140") {
        unmat2 = crate::glsl_es::Source::new(src).split_std140_mat2();
        padded = crate::glsl_es::Source::new(&unmat2).pad_std140_arrays();
        padded.as_str()
    } else {
        src
    };
    let mut frontend = naga::front::glsl::Frontend::default();
    let mut module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|error| Diagnostic::glsl(stage, entry, original, src, &error))?;
    if let Some(ep) = module.entry_points.first_mut() {
        ep.name = entry.to_string();
    }
    // GskGpu's texture-sampling helpers are `if/else if` chains with no final `else` (a valid-input-only
    // GLSL idiom), so naga's `glsl-in` fills the missing path with a bare `return;`
    // (`proc::ensure_block_returns`). In a value-returning function that bare return fails validation
    // (`InvalidReturnType(None)`). Replace each such fallthrough return with a zero-value return of the
    // function's result type — the path is unreachable for the values GskGpu emits.
    let mut shader = ShaderModule::new(&mut module);
    shader.lower_nonfinite_predicates();
    shader.default_bare_returns();
    // GskGpu declares functions top-down (`main` → `main_clip_*` → `run`) behind forward prototypes, which
    // GLSL permits but naga does not: naga assigns each function's handle at its first sighting (prototype
    // or definition) and its validator rejects any `Call` to a higher-indexed function
    // (`InvalidHandle(ForwardDependency)`). Reorder the parsed module's functions into call-graph
    // (callee-before-caller) order so every call points backward, as naga requires.
    shader.reorder_functions_topologically();
    // Dual-source blending: `crate::glsl_es` dropped the `index=` layout qualifier naga can't parse and
    // marked each `index>=1` fragment output with `BLEND_SRC1_SUFFIX`. Flip `second_blend_source` on those
    // outputs (and strip the marker) so the two same-location outputs validate as the dual-source pair.
    shader.fix_dual_source_blend();
    viewport::Shader::prepare(shader.module)?;
    let reflected = crate::reflect::ModuleUsage::from_module(shader.module);
    Ok((shader.wgsl()?, reflected))
}

mod kernel;
pub use kernel::Kernel;

#[cfg(test)]
#[path = "wgsl/tests.rs"]
mod gskgpu_tests;
