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
pub(crate) mod viewport;

#[hl_design::naming(reason = "diagnostic is the established noun for a shader compiler report")]
struct Diagnostic;

impl Diagnostic {
    const GLSL_CONTEXT_LIMIT: usize = 4096;

    fn kernel(message: impl Into<String>) -> GpuError {
        // The kernel/shader lowering surfaces its failures as a typed, message-carrying error so a program
        // outside the supported subset is a clean diagnostic, never a silent wrong-shader substitution.
        GpuError::Kernel(message.into())
    }

    fn glsl(
        stage: naga::ShaderStage,
        entry: &str,
        original: &str,
        normalized: &str,
        error: &naga::front::glsl::ParseErrors,
    ) -> GpuError {
        // Shader failures are rare and otherwise impossible to reproduce from naga's identifier-only
        // diagnostic. Emit the original and exact post-normalization inputs only on failure; successful
        // Chrome frames add no logging or formatting cost.
        hl_log::hl_error!(
            hl_log::tag::WGPU,
            "GLSL translation failed stage={stage:?} entry={entry} error={error:?}\n\
             --- original GLSL begin ---\n{original}\n--- original GLSL end ---\n\
             --- normalized GLSL begin ---\n{normalized}\n--- normalized GLSL end ---"
        );
        Self::kernel(Self::glsl_message(original, normalized, error))
    }

    fn glsl_message(
        original: &str,
        normalized: &str,
        error: &naga::front::glsl::ParseErrors,
    ) -> String {
        let mut message = String::new();
        Self::push_bounded(&mut message, &format!("glsl-in: {error:?}"));
        let names = error
            .errors
            .iter()
            .filter_map(|error| match &error.kind {
                naga::front::glsl::ErrorKind::UnknownVariable(name) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            return message;
        }
        Self::source_matches(&mut message, "original", original, &names);
        Self::source_matches(&mut message, "normalized", normalized, &names);
        message
    }

    fn source_matches(output: &mut String, label: &str, source: &str, names: &[&str]) {
        if output.len() >= Self::GLSL_CONTEXT_LIMIT {
            return;
        }
        let lines = source.lines().collect::<Vec<_>>();
        let mut selected = vec![false; lines.len()];
        for (index, line) in lines.iter().enumerate() {
            if names.iter().any(|name| line.contains(name)) {
                let start = index.saturating_sub(1);
                let end = (index + 2).min(lines.len());
                selected[start..end].fill(true);
            }
        }
        if !selected.iter().any(|selected| *selected) {
            return;
        }
        Self::push_bounded(output, &format!("\n{label} GLSL context:"));
        for (index, line) in lines.iter().enumerate() {
            if selected[index] {
                Self::push_bounded(output, &format!("\n{:>5} | {line}", index + 1));
            }
        }
    }

    fn push_bounded(output: &mut String, value: &str) {
        let remaining = Self::GLSL_CONTEXT_LIMIT.saturating_sub(output.len());
        if remaining == 0 {
            return;
        }
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&value[..end]);
    }
}

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
        Self::translate_reflect_layout(words, None)
    }

    pub fn translate_reflect_layout(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(words, layout, false)
    }

    pub fn translate_reflect_sample_shading(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
    ) -> Result<(String, crate::reflect::ModuleUsage)> {
        Self::translate_reflect_options(words, layout, true)
    }

    fn translate_reflect_options(
        words: &[u32],
        layout: Option<&hl_gpu::protocol::model::descriptor::PipelineLayout>,
        sample_shading: bool,
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
                let _base = match module.types[variable.ty].inner {
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
                    _ => continue,
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
        viewport::Shader::prepare(&mut module)?;
        let reflected = crate::reflect::ModuleUsage::from_module(&module);
        Ok((ShaderModule::new(&mut module).wgsl()?, reflected))
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
    // naga's `wgsl-out` has no `IsInf` emitter, so a shader using `isinf()` (Chrome's GLES fragment
    // shaders do) parses fine but NACKs at WGSL emission (`Unsupported relational function: IsInf`).
    // Rewrite `isinf(x)` → `(abs(x) > FLT_MAX)` textually before parsing. Unconditional and cheap: a
    // shader with no `isinf` is returned byte-for-byte, so non-isinf paths are unaffected.
    let deinf;
    let src = if src.contains("isinf") {
        deinf = crate::glsl_es::Source::new(src).rewrite_isinf();
        deinf.as_str()
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

struct ShaderModule<'a> {
    module: &'a mut naga::Module,
}

impl<'a> ShaderModule<'a> {
    fn new(module: &'a mut naga::Module) -> Self {
        Self { module }
    }

    /// Turn each fragment-output struct member named `<name>_hlbsrc1` (the marker `crate::glsl_es` stamps on a
    /// `layout(location=L, index=1)` dual-source output) into a real `@second_blend_source` output: set the
    /// member's `Binding::Location { second_blend_source: true }` and restore its original name. naga's `glsl-in`
    /// gathers a fragment stage's multiple `out` variables into the entry point's result STRUCT (one
    /// `StructMember` per output, each carrying a `Location` binding), so the fix rewrites that struct type in
    /// place (`UniqueArena::replace`). Modules with no such marker are left untouched.
    fn fix_dual_source_blend(&mut self) {
        let module = &mut *self.module;
        use naga::{Binding, Type, TypeInner};
        let suffix = crate::glsl_es::BLEND_SRC1_SUFFIX;
        let result_tys: Vec<naga::Handle<Type>> = module
            .entry_points
            .iter()
            .filter(|ep| ep.stage == naga::ShaderStage::Fragment)
            .filter_map(|ep| ep.function.result.as_ref().map(|r| r.ty))
            .collect();
        for ty_handle in result_tys {
            let rebuilt = {
                let Ok(ty) = module.types.get_handle(ty_handle) else {
                    continue;
                };
                let TypeInner::Struct { members, span } = &ty.inner else {
                    continue;
                };
                let mut new_members = members.clone();
                let mut changed = false;
                for m in new_members.iter_mut() {
                    let Some(name) = m.name.clone() else { continue };
                    if let Some(stripped) = name.strip_suffix(suffix) {
                        m.name = Some(stripped.to_string());
                        if let Some(Binding::Location {
                            second_blend_source,
                            ..
                        }) = &mut m.binding
                        {
                            *second_blend_source = true;
                            changed = true;
                        }
                    }
                }
                changed.then(|| Type {
                    name: ty.name.clone(),
                    inner: TypeInner::Struct {
                        members: new_members,
                        span: *span,
                    },
                })
            };
            if let Some(new_ty) = rebuilt {
                module.types.replace(ty_handle, new_ty);
            }
        }
    }

    /// Replace every bare `Return { value: None }` inside a value-returning function with a zero-value return
    /// of the declared result type. naga's `glsl-in` inserts such a bare return to terminate a control-flow
    /// path the GLSL left open (an `if/else if` with no final `else`); the validator then rejects it because
    /// the function must return a value. `Expression::ZeroValue` is pre-emitted, so no `Emit` is needed.
    fn default_bare_returns(&mut self) {
        fn fix(
            block: &mut [naga::Statement],
            exprs: &mut naga::Arena<naga::Expression>,
            ty: naga::Handle<naga::Type>,
        ) {
            use naga::Statement;
            for stmt in block.iter_mut() {
                match stmt {
                    Statement::Return { value } if value.is_none() => {
                        let zero =
                            exprs.append(naga::Expression::ZeroValue(ty), naga::Span::default());
                        *value = Some(zero);
                    }
                    Statement::Block(b) => fix(b, exprs, ty),
                    Statement::If { accept, reject, .. } => {
                        fix(accept, exprs, ty);
                        fix(reject, exprs, ty);
                    }
                    Statement::Switch { cases, .. } => {
                        for c in cases.iter_mut() {
                            fix(&mut c.body, exprs, ty);
                        }
                    }
                    Statement::Loop {
                        body, continuing, ..
                    } => {
                        fix(body, exprs, ty);
                        fix(continuing, exprs, ty);
                    }
                    _ => {}
                }
            }
        }
        for (_h, f) in self.module.functions.iter_mut() {
            if let Some(ty) = f.result.as_ref().map(|r| r.ty) {
                fix(&mut f.body, &mut f.expressions, ty);
            }
        }
        for ep in self.module.entry_points.iter_mut() {
            if let Some(ty) = ep.function.result.as_ref().map(|r| r.ty) {
                fix(&mut ep.function.body, &mut ep.function.expressions, ty);
            }
        }
    }

    /// Reorder `module.functions` into topological (callee-before-caller) order and remap every `Call` to the
    /// new handles, so naga's handle validator (which forbids a function calling a higher-indexed one) accepts
    /// modules whose source used forward prototypes. A no-op in effect for modules already in a valid order.
    fn reorder_functions_topologically(&mut self) {
        use naga::{Function, Handle, Span};

        let old = std::mem::take(&mut self.module.functions);
        let mut owned: Vec<Option<(Function, Span)>> = old
            .iter()
            .map(|(h, f)| Some((f.clone(), old.get_span(h))))
            .collect();
        let n = owned.len();

        // Call graph over old indices (a function's handle index equals its position in the old arena).
        let mut callees: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, slot) in owned.iter().enumerate() {
            Self::collect_call_targets(&slot.as_ref().expect("present").0.body, &mut callees[i]);
        }

        // Iterative postorder DFS: a node is emitted after all its callees, i.e. callees get lower new indices.
        // Back edges (would-be recursion, which naga/GLSL disallow anyway) are skipped so this always terminates.
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut state = vec![0u8; n]; // 0 = unseen, 1 = on stack, 2 = emitted
        for start in 0..n {
            if state[start] != 0 {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
            while let Some(&(node, ci)) = stack.last() {
                state[node] = 1;
                if ci < callees[node].len() {
                    stack.last_mut().expect("non-empty").1 += 1;
                    let next = callees[node][ci];
                    if state[next] == 0 {
                        stack.push((next, 0));
                    }
                } else {
                    if state[node] != 2 {
                        state[node] = 2;
                        order.push(node);
                    }
                    stack.pop();
                }
            }
        }

        let mut new_arena: naga::Arena<Function> = naga::Arena::default();
        let mut map: Vec<Option<Handle<Function>>> = vec![None; n];
        for &old_i in &order {
            let (f, span) = owned[old_i].take().expect("each function emitted once");
            map[old_i] = Some(new_arena.append(f, span));
        }

        for (_h, f) in new_arena.iter_mut() {
            Self::remap_call_targets(&mut f.body, &map);
            Self::remap_call_result_exprs(f, &map);
        }
        for ep in self.module.entry_points.iter_mut() {
            Self::remap_call_targets(&mut ep.function.body, &map);
            Self::remap_call_result_exprs(&mut ep.function, &map);
        }
        self.module.functions = new_arena;
    }

    /// Rewrite every `Expression::CallResult(function)` in `f`'s expression arena through `map`. The function
    /// handle a value-returning call yields is stored here (not only in `Statement::Call`), and naga's handle
    /// validator checks it against the enclosing function, so it must be remapped alongside the call statement.
    fn remap_call_result_exprs(
        f: &mut naga::Function,
        map: &[Option<naga::Handle<naga::Function>>],
    ) {
        for (_h, expr) in f.expressions.iter_mut() {
            if let naga::Expression::CallResult(function) = expr {
                if let Some(new_h) = map[function.index()] {
                    *function = new_h;
                }
            }
        }
    }

    /// Collect (deduplicated) old-index call targets reachable in `block`, recursing through nested blocks.
    fn collect_call_targets(block: &[naga::Statement], out: &mut Vec<usize>) {
        use naga::Statement;
        for stmt in block {
            match stmt {
                Statement::Call { function, .. } => {
                    let idx = function.index();
                    if !out.contains(&idx) {
                        out.push(idx);
                    }
                }
                Statement::Block(b) => Self::collect_call_targets(b, out),
                Statement::If { accept, reject, .. } => {
                    Self::collect_call_targets(accept, out);
                    Self::collect_call_targets(reject, out);
                }
                Statement::Switch { cases, .. } => {
                    for c in cases {
                        Self::collect_call_targets(&c.body, out);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    Self::collect_call_targets(body, out);
                    Self::collect_call_targets(continuing, out);
                }
                _ => {}
            }
        }
    }

    /// Rewrite every `Statement::Call` target in `block` through `map` (old index → new handle), recursing
    /// through nested blocks.
    fn remap_call_targets(
        block: &mut [naga::Statement],
        map: &[Option<naga::Handle<naga::Function>>],
    ) {
        use naga::Statement;
        for stmt in block.iter_mut() {
            match stmt {
                Statement::Call { function, .. } => {
                    if let Some(new_h) = map[function.index()] {
                        *function = new_h;
                    }
                }
                Statement::Block(b) => Self::remap_call_targets(b, map),
                Statement::If { accept, reject, .. } => {
                    Self::remap_call_targets(accept, map);
                    Self::remap_call_targets(reject, map);
                }
                Statement::Switch { cases, .. } => {
                    for c in cases.iter_mut() {
                        Self::remap_call_targets(&mut c.body, map);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    Self::remap_call_targets(body, map);
                    Self::remap_call_targets(continuing, map);
                }
                _ => {}
            }
        }
    }

    fn wgsl(&self) -> Result<String> {
        let module = &*self.module;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(module)
        .map_err(|e| Diagnostic::kernel(format!("validate: {e:?}")))?;
        naga::back::wgsl::write_string(module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| Diagnostic::kernel(format!("wgsl-out: {e}")))
    }
}

mod kernel;
pub use kernel::Kernel;

#[cfg(test)]
#[path = "wgsl/tests.rs"]
mod gskgpu_tests;
