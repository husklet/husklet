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
    gty, Inst, KernelProgram, Op, ATOM_ADD, ATOM_AND, ATOM_CAS, ATOM_EXCH, ATOM_MAX, ATOM_MIN,
    ATOM_OR, ATOM_XOR, BIT_AND, BIT_OR, CMP_EQ, CMP_GT, CMP_LE, CMP_LT, CMP_NE, CVT_F32_FROM_S32,
    CVT_S32_FROM_F32, CVT_S64_FROM_S32, SHIFT_LEFT, SR_CTAID_X, SR_CTAID_Y, SR_CTAID_Z,
    SR_NCTAID_X, SR_NCTAID_Y, SR_NTID_X, SR_NTID_Y, SR_NTID_Z, SR_TID_X, SR_TID_Y, SR_TID_Z,
};
use hl_gpu::{GpuError, Result};

#[hl_design::naming(reason = "diagnostic is the established noun for a shader compiler report")]
struct Diagnostic;

impl Diagnostic {
    fn kernel(message: impl Into<String>) -> GpuError {
        // The kernel/shader lowering surfaces its failures as a typed, message-carrying error so a program
        // outside the supported subset is a clean diagnostic, never a silent wrong-shader substitution.
        GpuError::Kernel(message.into())
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
        .map_err(|e| Diagnostic::kernel(format!("glsl-in: {e:?}")))?;
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

// ===================================================================================================
// kernel-IR → WGSL compute (ported from hl-gpu/src/ptx.rs::kernel_to_wgsl)
// ===================================================================================================

struct GlobalType(u8);

impl GlobalType {
    fn bytes(self) -> Result<u32> {
        match self.0 {
            gty::F32 | gty::U32 => Ok(4),
            gty::U64 => Err(Diagnostic::kernel(
                "wgsl lowering: 64-bit global load/store unsupported (elementwise subset)",
            )),
            other => Err(Diagnostic::kernel(format!(
                "wgsl lowering: unknown global type {other}"
            ))),
        }
    }
}

/// Static per-register pointer-region analysis: `out[r] == Some(region)` iff register `r` holds a device
/// address into that pointer-parameter region at its most recent definition. Forward, last-write-wins.
struct Analysis<'a> {
    program: &'a KernelProgram,
    regions: Vec<Option<u32>>,
}

impl<'a> Analysis<'a> {
    fn new(program: &'a KernelProgram) -> Self {
        let mut reg: Vec<Option<u32>> = vec![None; program.reg_count as usize];
        let of = |reg: &Vec<Option<u32>>, op: &Op| -> Option<u32> {
            match op {
                Op::Reg(r) => reg[*r as usize],
                _ => None,
            }
        };
        for inst in &program.insts {
            match inst {
                Inst::LdParam { d, param } => {
                    let p = &program.params[*param as usize];
                    reg[*d as usize] = if p.is_ptr { Some(p.region) } else { None };
                }
                Inst::MovReg { d, s } => reg[*d as usize] = reg[*s as usize],
                Inst::Cvta { d, s } => reg[*d as usize] = reg[*s as usize],
                Inst::IAdd { d, a, b, .. } | Inst::ISub { d, a, b, .. } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b));
                }
                Inst::IMad { d, a, b, c } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b)).or_else(|| of(&reg, c));
                }
                Inst::IMul { d, a, b, .. } => {
                    reg[*d as usize] = of(&reg, a).or_else(|| of(&reg, b))
                }
                Inst::MovImmI { d, .. }
                | Inst::MovImmF { d, .. }
                | Inst::MovSReg { d, .. }
                | Inst::LdGlobal { d, .. }
                | Inst::LdShared { d, .. }
                | Inst::Setp { d, .. }
                | Inst::Shift { d, .. }
                | Inst::BitOp { d, .. }
                | Inst::FAdd { d, .. }
                | Inst::FSub { d, .. }
                | Inst::FMul { d, .. }
                | Inst::FFma { d, .. }
                | Inst::Cvt { d, .. } => reg[*d as usize] = None,
                Inst::AtomGlobal { d, .. } | Inst::AtomShared { d, .. } => {
                    if let Some(dr) = d {
                        reg[*dr as usize] = None;
                    }
                }
                Inst::StGlobal { .. }
                | Inst::StShared { .. }
                | Inst::Bra { .. }
                | Inst::Bar
                | Inst::Ret
                | Inst::Nop => {}
            }
        }
        Self {
            program,
            regions: reg,
        }
    }

    /// Which pointer regions are accessed atomically → those storage buffers must be `array<atomic<u32>>`.
    fn atomic_regions(&self) -> Result<Vec<bool>> {
        let mut atomic = vec![false; self.program.num_regions as usize];
        for inst in &self.program.insts {
            if let Inst::AtomGlobal { addr, .. } = inst {
                let r = self.regions[*addr as usize].ok_or_else(|| {
                    Diagnostic::kernel(
                        "wgsl lowering: atomic through a value with no static pointer region",
                    )
                })?;
                atomic[r as usize] = true;
            }
        }
        Ok(atomic)
    }
}

/// A special-register id → the WGSL builtin/constant expression that yields its value.
struct SpecialRegister(u8);

impl SpecialRegister {
    fn expression(self, block: [u32; 3]) -> String {
        match self.0 {
            SR_TID_X => "lid.x".into(),
            SR_TID_Y => "lid.y".into(),
            SR_TID_Z => "lid.z".into(),
            SR_NTID_X => format!("{}u", block[0]),
            SR_NTID_Y => format!("{}u", block[1]),
            SR_NTID_Z => format!("{}u", block[2]),
            SR_CTAID_X => "wid.x".into(),
            SR_CTAID_Y => "wid.y".into(),
            SR_CTAID_Z => "wid.z".into(),
            SR_NCTAID_X => "nwg.x".into(),
            SR_NCTAID_Y => "nwg.y".into(),
            _ => "nwg.z".into(), // SR_NCTAID_Z
        }
    }
}

struct Operand<'a>(&'a Op);

impl Operand<'_> {
    fn u32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("r{r}"),
            Op::ImmI(i) => format!("{}u", *i as u32),
            Op::ImmF(b) => format!("{b}u"),
        }
    }

    fn i32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("bitcast<i32>(r{r})"),
            Op::ImmI(i) => format!("i32({})", *i as i32),
            Op::ImmF(b) => format!("bitcast<i32>({b}u)"),
        }
    }

    fn f32(&self) -> String {
        match self.0 {
            Op::Reg(r) => format!("bitcast<f32>(r{r})"),
            Op::ImmF(b) => format!("bitcast<f32>({b}u)"),
            Op::ImmI(i) => format!("f32({i})"),
        }
    }
}

/// Lower a compiled [`KernelProgram`] to a WGSL compute shader whose entry point is `prog.entry`. Returns
/// a typed error for any instruction outside the elementwise subset this lowering supports.
pub struct Kernel;

impl Kernel {
    pub fn translate(prog: &KernelProgram) -> Result<String> {
        let analysis = Analysis::new(prog);
        let region = &analysis.regions;
        let region_of = |addr: u16| -> Result<u32> {
            region[addr as usize].ok_or_else(|| {
                Diagnostic::kernel(
                    "wgsl lowering: global access through a value with no static pointer region",
                )
            })
        };
        let atomic = analysis.atomic_regions()?;
        let shared_atomic = prog
            .insts
            .iter()
            .any(|i| matches!(i, Inst::AtomShared { .. }));
        let coop = prog.insts.iter().any(|i| matches!(i, Inst::Bar));

        let shared_idx = |base: &Op, off: i64, elem: u32| {
            format!("(({} + {}u) / {elem}u)", Operand(base).u32(), off as u32)
        };

        let mut s = String::new();
        s.push_str("@group(0) @binding(0) var<storage, read> params: array<u32>;\n");
        for r in 0..prog.num_regions {
            let elem_ty = if atomic[r as usize] {
                "atomic<u32>"
            } else {
                "u32"
            };
            s.push_str(&format!(
                "@group(0) @binding({}) var<storage, read_write> region{r}: array<{elem_ty}>;\n",
                r + 1
            ));
        }
        if prog.shared_bytes > 0 {
            let words = (prog.shared_bytes / 4).max(1);
            let elem_ty = if shared_atomic { "atomic<u32>" } else { "u32" };
            s.push_str(&format!(
                "var<workgroup> shmem: array<{elem_ty}, {words}>;\n"
            ));
        }
        if coop {
            s.push_str("var<workgroup> hl_retired_count: atomic<u32>;\n");
        }
        s.push('\n');

        let [bx, by, bz] = prog.block;
        let total_threads = bx.max(1) * by.max(1) * bz.max(1);
        s.push_str(&format!(
            "@compute @workgroup_size({}, {}, {})\n",
            bx.max(1),
            by.max(1),
            bz.max(1)
        ));
        s.push_str(&format!("fn {}(\n", prog.entry));
        s.push_str("    @builtin(local_invocation_id) lid: vec3<u32>,\n");
        if coop {
            s.push_str("    @builtin(local_invocation_index) lidx: u32,\n");
        }
        s.push_str("    @builtin(workgroup_id) wid: vec3<u32>,\n");
        s.push_str("    @builtin(num_workgroups) nwg: vec3<u32>,\n");
        s.push_str(") {\n");

        for r in 0..prog.reg_count {
            s.push_str(&format!("    var r{r}: u32 = 0u;\n"));
        }

        if coop {
            s.push_str("    if (lidx == 0u) { atomicStore(&hl_retired_count, 0u); }\n");
            s.push_str("    workgroupBarrier();\n");
            s.push_str("    var pc: i32 = 0;\n");
            s.push_str("    var hl_retired: bool = false;\n");
            s.push_str("    loop {\n");
            s.push_str("        if (!hl_retired) {\n");
            s.push_str("            switch pc {\n");
        } else {
            s.push_str("    var pc: i32 = 0;\n");
            s.push_str("    loop {\n");
            s.push_str("        if (pc < 0) { break; }\n");
            s.push_str("        switch pc {\n");
        }
        let indent = if coop {
            "                "
        } else {
            "            "
        };
        let retire = if coop {
            "atomicAdd(&hl_retired_count, 1u); hl_retired = true; pc = -1;"
        } else {
            "pc = -1;"
        };
        for (k, inst) in prog.insts.iter().enumerate() {
            let mut body = String::new();
            let mut branched = false;
            match inst {
                Inst::Ret => {
                    body.push_str(retire);
                    branched = true;
                }
                Inst::Nop => {}
                Inst::Bar => {}
                Inst::MovImmI { d, imm } => body.push_str(&format!("r{d} = {}u;", *imm as u32)),
                Inst::MovImmF { d, bits } => body.push_str(&format!("r{d} = {bits}u;")),
                Inst::MovReg { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
                Inst::MovSReg { d, sreg } => body.push_str(&format!(
                    "r{d} = {};",
                    SpecialRegister(*sreg).expression(prog.block)
                )),
                Inst::LdParam { d, param } => {
                    let p = &prog.params[*param as usize];
                    if p.is_ptr {
                        body.push_str(&format!("r{d} = 0u;"));
                    } else {
                        body.push_str(&format!("r{d} = params[{}u];", p.offset / 4));
                    }
                }
                Inst::Cvta { d, s: src } => body.push_str(&format!("r{d} = r{src};")),
                Inst::IAdd { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} + {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::ISub { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} - {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::IMul { d, a, b, .. } => body.push_str(&format!(
                    "r{d} = {} * {};",
                    Operand(a).u32(),
                    Operand(b).u32()
                )),
                Inst::IMad { d, a, b, c } => body.push_str(&format!(
                    "r{d} = {} * {} + {};",
                    Operand(a).u32(),
                    Operand(b).u32(),
                    Operand(c).u32()
                )),
                Inst::Shift {
                    d,
                    a,
                    b,
                    dir,
                    unsigned,
                } => {
                    let sh = format!("({} & 31u)", Operand(b).u32());
                    let e = match *dir {
                        SHIFT_LEFT => format!("{} << {sh}", Operand(a).u32()),
                        _ if *unsigned => format!("{} >> {sh}", Operand(a).u32()),
                        _ => format!("bitcast<u32>({} >> {sh})", Operand(a).i32()),
                    };
                    body.push_str(&format!("r{d} = {e};"));
                }
                Inst::BitOp { d, a, b, op } => {
                    let sym = match *op {
                        BIT_AND => "&",
                        BIT_OR => "|",
                        _ => "^",
                    };
                    body.push_str(&format!(
                        "r{d} = {} {sym} {};",
                        Operand(a).u32(),
                        Operand(b).u32()
                    ));
                }
                Inst::Setp {
                    d,
                    a,
                    b,
                    cmp,
                    unsigned,
                } => {
                    let sym = match *cmp {
                        CMP_EQ => "==",
                        CMP_NE => "!=",
                        CMP_LT => "<",
                        CMP_LE => "<=",
                        CMP_GT => ">",
                        _ => ">=", // CMP_GE
                    };
                    let cond = if *unsigned {
                        format!("{} {sym} {}", Operand(a).u32(), Operand(b).u32())
                    } else {
                        format!("{} {sym} {}", Operand(a).i32(), Operand(b).i32())
                    };
                    body.push_str(&format!("r{d} = select(0u, 1u, {cond});"));
                }
                Inst::Bra { target, pred } => {
                    match pred {
                        None => body.push_str(&format!("pc = {target};")),
                        Some((p, neg)) => {
                            let cond = if *neg {
                                format!("r{p} == 0u")
                            } else {
                                format!("r{p} != 0u")
                            };
                            body.push_str(&format!(
                                "if ({cond}) {{ pc = {target}; }} else {{ pc = {}; }}",
                                k + 1
                            ));
                        }
                    }
                    branched = true;
                }
                Inst::LdGlobal { d, addr, off, ty } => {
                    let r = region_of(*addr)?;
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                    if atomic[r as usize] {
                        body.push_str(&format!("r{d} = atomicLoad(&region{r}[{idx}]);"));
                    } else {
                        body.push_str(&format!("r{d} = region{r}[{idx}];"));
                    }
                }
                Inst::StGlobal { addr, off, src, ty } => {
                    let r = region_of(*addr)?;
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = format!("(r{addr} + {}u) / {elem}u", *off as u32);
                    if atomic[r as usize] {
                        body.push_str(&format!(
                            "atomicStore(&region{r}[{idx}], {});",
                            Operand(src).u32()
                        ));
                    } else {
                        body.push_str(&format!("region{r}[{idx}] = {};", Operand(src).u32()));
                    }
                }
                Inst::LdShared { d, base, off, ty } => {
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = shared_idx(base, *off, elem);
                    if shared_atomic {
                        body.push_str(&format!("r{d} = atomicLoad(&shmem[{idx}]);"));
                    } else {
                        body.push_str(&format!("r{d} = shmem[{idx}];"));
                    }
                }
                Inst::StShared { base, off, src, ty } => {
                    let elem = GlobalType(*ty).bytes()?;
                    let idx = shared_idx(base, *off, elem);
                    if shared_atomic {
                        body.push_str(&format!(
                            "atomicStore(&shmem[{idx}], {});",
                            Operand(src).u32()
                        ));
                    } else {
                        body.push_str(&format!("shmem[{idx}] = {};", Operand(src).u32()));
                    }
                }
                Inst::AtomGlobal {
                    d,
                    addr,
                    off,
                    op,
                    cmp,
                    val,
                    unsigned,
                } => {
                    let r = region_of(*addr)?;
                    let idx = format!("(r{addr} + {}u) / 4u", *off as u32);
                    let ptr = format!("&region{r}[{idx}]");
                    emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
                }
                Inst::AtomShared {
                    d,
                    base,
                    off,
                    op,
                    cmp,
                    val,
                    unsigned,
                } => {
                    if !shared_atomic {
                        return Err(Diagnostic::kernel(
                            "wgsl lowering: atom.shared requires an atomic shared array",
                        ));
                    }
                    let idx = shared_idx(base, *off, 4);
                    let ptr = format!("&shmem[{idx}]");
                    emit_atomic(&mut body, &ptr, *op, cmp, val, *d, *unsigned)?;
                }
                Inst::FAdd { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} + {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FSub { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} - {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FMul { d, a, b } => body.push_str(&format!(
                    "r{d} = bitcast<u32>({} * {});",
                    Operand(a).f32(),
                    Operand(b).f32()
                )),
                Inst::FFma { d, a, b, c } => body.push_str(&format!(
                    "r{d} = bitcast<u32>(fma({}, {}, {}));",
                    Operand(a).f32(),
                    Operand(b).f32(),
                    Operand(c).f32()
                )),
                Inst::Cvt { d, s: src, kind } => {
                    let e = match *kind {
                        CVT_F32_FROM_S32 => format!("bitcast<u32>(f32({}))", Operand(src).i32()),
                        CVT_S64_FROM_S32 => Operand(src).u32(),
                        CVT_S32_FROM_F32 => format!("bitcast<u32>(i32({}))", Operand(src).f32()),
                        _ => Operand(src).u32(),
                    };
                    body.push_str(&format!("r{d} = {e};"));
                }
            }
            if !branched {
                body.push_str(&format!(" pc = {};", k + 1));
            }
            s.push_str(&format!("{indent}case {k}: {{ {body} }}\n"));
        }
        s.push_str(&format!("{indent}default: {{ {retire} }}\n"));
        if coop {
            s.push_str("            }\n");
            s.push_str("        }\n");
            s.push_str("        workgroupBarrier();\n");
            s.push_str(&format!(
                "        if (atomicLoad(&hl_retired_count) == {total_threads}u) {{ break; }}\n"
            ));
            s.push_str("    }\n}\n");
        } else {
            s.push_str("        }\n    }\n}\n");
        }
        Ok(s)
    }
}

#[cfg(test)]
mod gskgpu_tests {
    //! The GskGpu (GTK4 "gl") / ANGLE unblock: a representative GLSL-ES texture-op pair — `#version 320
    //! es`, `precision`, `gl_VertexID`, a combined `sampler2D` GLOBAL, and the hard case, a helper function
    //! that takes a `sampler2D` PARAMETER — is rejected wholesale by naga's `glsl-in` (FAIL-before) but
    //! compiles to real WGSL through [`glsl_to_wgsl`]'s ES-normalize + sampler-split (PASS-after), with the
    //! texture and sampler landing at the coordinated bindings the driver binds to. Mirrors the
    //! `spirv_split.rs` FAIL-before/PASS-after proof, sourced from GLSL instead of app SPIR-V.
    use super::*;

    // A GskGpu-shaped vertex shader: computes position from `gl_VertexID` (no position attribute), reads a
    // `binding=0` push-constant-style UBO via `push.`, forwards a uv varying.
    const GSK_VERT: &str = r#"#version 320 es
precision highp float;
layout(std140, binding = 0) uniform PushConstants { mat4 mvp; vec4 rect; } push;
out vec2 vUV;
void main() {
    int id = gl_VertexID;
    vec2 corner = vec2(float(id & 1), float((id >> 1) & 1));
    vUV = corner;
    gl_Position = push.mvp * vec4(push.rect.xy + corner * push.rect.zw, 0.0, 1.0);
}
"#;

    // A GskGpu-shaped fragment shader: a combined `sampler2D` global sampled THROUGH a helper that takes a
    // `sampler2D` parameter — the construct the spec calls a hard naga limit.
    const GSK_FRAG: &str = r#"#version 320 es
precision highp float;
uniform sampler2D uTexture;
in vec2 vUV;
layout(location = 0) out vec4 outColor;
vec4 gsk_texture(sampler2D tex, vec2 p) {
    return texture(tex, p);
}
void main() {
    outColor = gsk_texture(uTexture, vUV);
}
"#;

    fn naga_direct(src: &str, stage: naga::ShaderStage) -> Result<()> {
        let mut f = naga::front::glsl::Frontend::default();
        f.parse(&naga::front::glsl::Options::from(stage), src)
            .map(|_| ())
            .map_err(|e| Diagnostic::kernel(format!("{e:?}")))
    }

    #[test]
    fn gskgpu_pair_fails_naga_directly_but_compiles_through_glsl_to_wgsl() {
        // FAIL-BEFORE: naga's glsl-in rejects both stages as-is (ES version + gl_VertexID; combined
        // sampler global + sampler2D parameter).
        assert!(
            naga_direct(GSK_VERT, naga::ShaderStage::Vertex).is_err(),
            "vert must fail raw naga"
        );
        assert!(
            naga_direct(GSK_FRAG, naga::ShaderStage::Fragment).is_err(),
            "frag must fail raw naga"
        );

        // PASS-AFTER: the ES-normalize + sampler-split route compiles both to real WGSL.
        let vwgsl = glsl_to_wgsl(GSK_VERT, naga::ShaderStage::Vertex, "vmain")
            .expect("GskGpu vertex must compile through the ES route");
        let fwgsl = glsl_to_wgsl(GSK_FRAG, naga::ShaderStage::Fragment, "fmain")
            .expect("GskGpu fragment must compile through the ES route");

        // Vertex: gl_VertexID lowered to the vertex-index builtin, entry renamed.
        assert!(
            vwgsl.contains("vertex_index"),
            "vertex_index builtin expected: {vwgsl}"
        );
        assert!(vwgsl.contains("vmain"), "entry rename expected: {vwgsl}");

        // Fragment: the combined sampler became a SEPARATE texture_2d + sampler at the coordinated bindings
        // (sampler 0 → texture @binding(1), sampler @binding(2)), so the driver's bind-group entries match.
        assert!(
            fwgsl.contains("texture_2d"),
            "expected a separate texture_2d: {fwgsl}"
        );
        assert!(
            fwgsl.contains(": sampler"),
            "expected a separate sampler: {fwgsl}"
        );
        assert!(
            fwgsl.contains("@binding(1)"),
            "texture must reflect at binding 1: {fwgsl}"
        );
        assert!(
            fwgsl.contains("@binding(2)"),
            "sampler must reflect at binding 2: {fwgsl}"
        );
        assert!(
            fwgsl.contains("textureSample"),
            "the helper's texture() lowered to a sample: {fwgsl}"
        );
    }

    // A vertex program that reproduces the *structural* GskGpu constructs the live GTK4 source hit past the
    // sampler/version gate: the `#if __VERSION__`-gated UBO binding, `gl_VertexID` hidden in the
    // `GSK_VERTEX_INDEX` macro, the location-dropping `IN`/`PASS` macros, a returning `switch`, and — the
    // two naga *module* passes — a forward-declared function called before its definition, and a
    // value-returning `if/else if` helper with no final `else`.
    const GSK_REAL_VERT: &str = r#"#version 320 es
#define GSK_GLES 1
void main_clip_none (void);
precision highp float;
#if __VERSION__ < 420 || (defined(GSK_GLES) && __VERSION__ < 310)
layout(std140)
#else
layout(std140, binding = 0)
#endif
uniform PushConstants { mat4 mvp; } push;
#define GSK_VERTEX_INDEX gl_VertexID
#define IN(_loc) in
#define PASS(_loc) out
IN(0) vec4 in_rect;
IN(1) vec4 in_color;
PASS(0) vec2 _uv;
int classify (uint op)
{
  switch (op)
    {
    case 0u:
      return 1;
    case 1u:
    case 2u:
      return 2;
    default:
      return 0;
    }
}
vec4 pick (int op)
{
  if (op == 1)
    return in_rect;
  else if (op == 2)
    return in_color;
}
void main_clip_none (void)
{
  int c = classify (uint (GSK_VERTEX_INDEX & 3));
  _uv = pick (c).xy;
  gl_Position = push.mvp * (in_rect + in_color);
}
void main ()
{
  main_clip_none ();
}
"#;

    #[test]
    fn gskgpu_real_structural_constructs_compile_through_glsl_to_wgsl() {
        // FAIL-BEFORE: raw naga rejects the ES version / gl_VertexID outright.
        assert!(
            naga_direct(GSK_REAL_VERT, naga::ShaderStage::Vertex).is_err(),
            "must fail raw naga"
        );

        // PASS-AFTER: the ES lowering + the two module passes (forward-decl reorder, bare-return default)
        // produce a validated WGSL vertex program.
        let wgsl = glsl_to_wgsl(GSK_REAL_VERT, naga::ShaderStage::Vertex, "vmain")
            .expect("real GskGpu structural constructs must compile through the ES route");
        assert!(
            wgsl.contains("vertex_index"),
            "gl_VertexID → vertex_index builtin: {wgsl}"
        );
        assert!(wgsl.contains("vmain"), "entry renamed: {wgsl}");
        // The forward-declared `main_clip_none` and its callees resolved (no forward-dependency).
        assert!(
            wgsl.contains("main_clip_none"),
            "forward-declared fn present: {wgsl}"
        );
    }

    // The GskGpu border/texture ops declare AGGREGATE interface members naga rejects as a single located
    // slot — a `mat3x4` vertex attribute and a `RoundedRect` (== `vec4[3]`) inter-stage varying — and, like
    // every GskGpu shader, define `main` (from the shared common.glsl) BEFORE the per-op I/O declarations.
    // The real live vertex stop was `EntryPoint { stage: Vertex, source: Argument(1, NotIOShareableType) }`.
    const GSK_AGG_VERT: &str = r#"#version 320 es
#define GSK_GLES 1
precision highp float;
#define RoundedRect vec4[3]
#if __VERSION__ < 420 || (defined(GSK_GLES) && __VERSION__ < 310)
layout(std140)
#else
layout(std140, binding = 0)
#endif
uniform PushConstants { mat4 mvp; } push;
#define GSK_VERTEX_INDEX gl_VertexID
#define IN(_loc) in
#define PASS(_loc) out
#define PASS_FLAT(_loc) flat out
void run (out vec2 pos);
void main (void)
{
  vec2 pos;
  run (pos);
  gl_Position = push.mvp * vec4 (pos, 0.0, 1.0);
}
IN(0) mat3x4 in_outline;
IN(3) vec4 in_color;
PASS(0) vec2 _pos;
PASS_FLAT(1) vec4 _color;
PASS_FLAT(2) RoundedRect _outline;
RoundedRect make (mat3x4 m)
{
  return RoundedRect (m[0], m[1], m[2]);
}
void run (out vec2 pos)
{
  RoundedRect o = make (in_outline);
  pos = o[0].xy + float (GSK_VERTEX_INDEX);
  _pos = pos;
  _outline = o;
  _color = in_color;
}
"#;

    // The matching fragment stage: the SAME `RoundedRect` varying arrives as a `flat in` array and is read
    // through a helper — the input-direction half of the aggregate split (reconstructed at the top of main).
    const GSK_AGG_FRAG: &str = r#"#version 320 es
precision highp float;
#define RoundedRect vec4[3]
#define PASS(_loc) in
#define PASS_FLAT(_loc) flat in
PASS(0) vec2 _pos;
PASS_FLAT(1) vec4 _color;
PASS_FLAT(2) RoundedRect _outline;
layout(location = 0) out vec4 out_color;
float coverage (RoundedRect r, vec2 p)
{
  return clamp (r[0].x - p.x, 0.0, 1.0);
}
void main (void)
{
  out_color = _color * coverage (_outline, _pos);
}
"#;

    #[test]
    fn gskgpu_aggregate_interface_members_split_into_ioshareable_slots() {
        // FAIL-BEFORE: naga rejects `mat3x4` / array interface members at validation (NotIOShareableType).
        // (The parse itself succeeds, so `naga_direct` catches the ES version; validation is the real wall.)
        assert!(
            naga_direct(GSK_AGG_VERT, naga::ShaderStage::Vertex).is_err(),
            "vert must fail raw naga"
        );

        // PASS-AFTER: the `mat3x4` attribute is split into 3 vec4 columns at consecutive locations and the
        // `vec4[3]` varying into 3 flat vec4 slots, each an IO-shareable vector; the aggregates survive as
        // private globals bridged inside `main`, so the whole vertex program validates.
        let vwgsl = glsl_to_wgsl(GSK_AGG_VERT, naga::ShaderStage::Vertex, "vmain")
            .expect("aggregate vertex inputs/varyings must split and compile");
        assert!(vwgsl.contains("vmain"), "entry renamed: {vwgsl}");
        // The matrix input became per-column vector slots (no matrix survives as an entry-point argument).
        assert!(
            vwgsl.contains("in_outline_hlio0"),
            "mat3x4 input split into column slots: {vwgsl}"
        );
        assert!(
            vwgsl.contains("_outline_hlio0"),
            "array varying split into vector slots: {vwgsl}"
        );

        // The fragment stage reads the same split array varying (reconstructed on entry) and compiles.
        let fwgsl = glsl_to_wgsl(GSK_AGG_FRAG, naga::ShaderStage::Fragment, "fmain")
            .expect("aggregate fragment varyings must split and compile");
        assert!(fwgsl.contains("fmain"), "entry renamed: {fwgsl}");
        assert!(
            fwgsl.contains("_outline_hlio0"),
            "array varying split on the fragment side: {fwgsl}"
        );
    }

    // A GLES fragment shader that gates its output color on `isinf()` — the exact Chrome shape that NACKed
    // the executor with `Kernel("wgsl-out: Unsupported relational function: IsInf")`.
    const ISINF_FRAG: &str = r#"#version 320 es
precision highp float;
in float vScale;
layout(location = 0) out vec4 outColor;
void main() {
    float s = 1.0 / vScale;
    if (isinf(s)) {
        outColor = vec4(1.0, 0.0, 0.0, 1.0);
    } else {
        outColor = vec4(0.0, s, 0.0, 1.0);
    }
}
"#;

    #[test]
    fn isinf_fails_wgsl_out_directly_but_compiles_through_glsl_to_wgsl() {
        // FAIL-BEFORE: naga's glsl-in ACCEPTS `isinf` (lowering it to a `RelationalFunction::IsInf`), but
        // its `wgsl-out` writer has no emitter for it — validate-then-write NACKs with the IsInf message.
        // (Feed the ES-normalized text so the ONLY remaining gap is the isinf builtin itself.)
        let normalized =
            crate::glsl_es::Source::new(ISINF_FRAG).normalize(naga::ShaderStage::Fragment);
        let mut f = naga::front::glsl::Frontend::default();
        let mut module = f
            .parse(
                &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
                &normalized,
            )
            .expect("glsl-in accepts isinf");
        match ShaderModule::new(&mut module).wgsl() {
            Err(GpuError::Kernel(m)) => {
                assert!(
                    m.contains("IsInf"),
                    "expected the IsInf wgsl-out gap, got: {m}"
                )
            }
            other => panic!("expected the IsInf wgsl-out failure, got {other:?}"),
        }

        // PASS-AFTER: the `isinf` → `(abs(x) > FLT_MAX)` rewrite compiles the shader to real WGSL — no
        // `isinf`/`IsInf` survives and the finite-max-bound `abs(...)` is present.
        let wgsl = glsl_to_wgsl(ISINF_FRAG, naga::ShaderStage::Fragment, "fmain")
            .expect("isinf fragment must compile through the rewrite");
        assert!(
            !wgsl.contains("isinf") && !wgsl.contains("isInf") && !wgsl.contains("IsInf"),
            "no isinf survives to WGSL: {wgsl}"
        );
        assert!(
            wgsl.contains("abs("),
            "the finite-max-bound rewrite is present: {wgsl}"
        );
    }

    #[test]
    fn rewrite_isinf_is_exact_and_leaves_isinf_free_source_untouched() {
        use crate::glsl_es::Source;
        // A shader with no isinf is returned byte-for-byte (fast path).
        let plain = "void main() { float x = 1.0; }";
        assert_eq!(
            Source::new(plain).rewrite_isinf(),
            plain,
            "isinf-free source is untouched"
        );

        // Scalar isinf on an expression argument, with a nested isinf, both rewritten to the abs bound; no
        // `isinf` token remains and the `isinfx` identifier (a false prefix) is NOT touched.
        let src =
            "bool a = isinf(u.v * 2.0); bool b = isinf(isinf(w) ? 1.0 : x); float isinfx = 3.0;";
        let out = Source::new(src).rewrite_isinf();
        assert!(
            !out.contains("isinf("),
            "every isinf() call rewritten: {out}"
        );
        assert!(
            out.contains("(abs(u.v * 2.0) > 3.40282347e38)"),
            "scalar arg rewritten: {out}"
        );
        assert!(
            out.contains("isinfx"),
            "the `isinfx` identifier is not a false match: {out}"
        );
    }
}

/// Emit a WGSL atomic read-modify-write on `ptr` (an `atomic<u32>`), optionally capturing the old value.
fn emit_atomic(
    body: &mut String,
    ptr: &str,
    op: u8,
    cmp: &Op,
    val: &Op,
    d: Option<u16>,
    unsigned: bool,
) -> Result<()> {
    let dst = |body: &mut String, expr: &str| match d {
        Some(dr) => body.push_str(&format!("r{dr} = {expr};")),
        None => body.push_str(&format!("{expr};")),
    };
    match op {
        ATOM_ADD => dst(body, &format!("atomicAdd({ptr}, {})", Operand(val).u32())),
        ATOM_AND => dst(body, &format!("atomicAnd({ptr}, {})", Operand(val).u32())),
        ATOM_OR => dst(body, &format!("atomicOr({ptr}, {})", Operand(val).u32())),
        ATOM_XOR => dst(body, &format!("atomicXor({ptr}, {})", Operand(val).u32())),
        ATOM_EXCH => dst(
            body,
            &format!("atomicExchange({ptr}, {})", Operand(val).u32()),
        ),
        ATOM_MIN if unsigned => dst(body, &format!("atomicMin({ptr}, {})", Operand(val).u32())),
        ATOM_MAX if unsigned => dst(body, &format!("atomicMax({ptr}, {})", Operand(val).u32())),
        ATOM_MIN | ATOM_MAX => {
            return Err(Diagnostic::kernel(
                "wgsl lowering: signed atomic min/max unsupported (atomic word is u32)",
            ))
        }
        ATOM_CAS => {
            let (c, v) = (Operand(cmp).u32(), Operand(val).u32());
            match d {
                Some(dr) => body.push_str(&format!(
                    "loop {{ let res = atomicCompareExchangeWeak({ptr}, {c}, {v}); \
                     if (res.exchanged || res.old_value != {c}) {{ r{dr} = res.old_value; break; }} }}"
                )),
                None => body.push_str(&format!(
                    "loop {{ let res = atomicCompareExchangeWeak({ptr}, {c}, {v}); \
                     if (res.exchanged || res.old_value != {c}) {{ break; }} }}"
                )),
            }
        }
        _ => return Err(Diagnostic::kernel("wgsl lowering: unknown atomic op")),
    }
    Ok(())
}
