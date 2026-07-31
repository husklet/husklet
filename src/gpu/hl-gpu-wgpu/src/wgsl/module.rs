//! In-place repairs to a parsed naga module, plus WGSL emission.
//!
//! Everything here fixes a shape naga's front ends produce but its validator or `wgsl-out` rejects
//! (bare returns on open control-flow paths, forward-declared functions, dual-source blend outputs).
//! Both the SPIR-V and GLSL front ends in [`super`] run these before writing WGSL.

use hl_gpu::Result;

use super::diagnostic::Diagnostic;

pub(super) struct ShaderModule<'a> {
    pub(super) module: &'a mut naga::Module,
}

impl<'a> ShaderModule<'a> {
    pub(super) fn new(module: &'a mut naga::Module) -> Self {
        Self { module }
    }

    /// Turn each fragment-output struct member named `<name>_hlbsrc1` (the marker `crate::glsl_es` stamps on a
    /// `layout(location=L, index=1)` dual-source output) into a real `@second_blend_source` output: set the
    /// member's `Binding::Location { second_blend_source: true }` and restore its original name. naga's `glsl-in`
    /// gathers a fragment stage's multiple `out` variables into the entry point's result STRUCT (one
    /// `StructMember` per output, each carrying a `Location` binding), so the fix rewrites that struct type in
    /// place (`UniqueArena::replace`). Modules with no such marker are left untouched.
    pub(super) fn fix_dual_source_blend(&mut self) {
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
    pub(super) fn default_bare_returns(&mut self) {
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
    pub(super) fn reorder_functions_topologically(&mut self) {
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

    /// Lower `isInf`/`isNan` to integer tests on the IEEE-754 bit pattern (see [`super::nonfinite`]).
    /// naga's `wgsl-out` can emit neither, so a module still carrying one is refused at emission — which
    /// is what a SPIR-V payload using either predicate ran into, since the GLSL route's textual rewrite
    /// cannot reach it. A module using neither is left untouched.
    pub(super) fn lower_nonfinite_predicates(&mut self) {
        super::nonfinite::NonFinite::lower(self.module);
    }

    pub(super) fn wgsl(&self) -> Result<String> {
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
