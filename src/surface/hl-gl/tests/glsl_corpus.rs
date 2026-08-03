//! The GLSL-ES shader-corpus battery — bulletproofing the `adapter/glsl.rs` translator against a broad,
//! ANGLE/GskGpu-shaped corpus. ANGLE (Chrome) emits hundreds of intricate GLSL-ES shaders; if the driver's
//! reflect-and-regenerate translator chokes on any construct, the compiled GLSL naga rejects and Chrome
//! renders blank. This battery drives each corpus shader through the REAL shim shader path (create program
//! → glShaderSource ES source → compile → link → forward the shader payload) and then compiles the emitted
//! GLSL through the SAME host route the wgpu executor uses (naga `glsl-in` → `default_bare_returns` →
//! topological function reorder → validate → `wgsl-out`). A translator gap therefore surfaces as a REAL
//! naga error, not a structural guess.
//!
//! ## Two host routes, mirrored exactly
//!
//! The executor compiles a forwarded shader one of two ways (see `hl-gpu-wgpu::wgsl::glsl_to_wgsl`):
//!   * **desktop route** — the driver's `translate_render` output is already naga-acceptable desktop GLSL
//!     460, so `is_es_glsl` is false and naga's `glsl-in` runs DIRECTLY. This battery reproduces that route
//!     ([`host::compile_desktop`]) and asserts every `Path::Translate` corpus entry compiles + validates.
//!     This is the driver's own territory — a failure here is an `adapter/glsl.rs` bug that gets fixed.
//!   * **ES route** — a `gl_VertexID`/`gl_InstanceID` vertex-puller or a combined-sampler helper parameter
//!     is forwarded VERBATIM to the executor's `glsl_es::normalize` (which the driver cannot reproduce). A
//!     `Path::Verbatim` corpus entry asserts the driver correctly ROUTES it verbatim (the executor's ES
//!     route, exercised in `hl-gpu-wgpu`, owns the compile). Mis-routing here IS a driver bug.
//!
//! ## Real render (exact pixel)
//!
//! Where a shader is a position + vertex-color passthrough the reference `CpuExecutor` can rasterize, the
//! frame is executed on the in-process CPU oracle and an exact pixel is asserted — a real render, not just a
//! compile. (The CPU oracle rasterizes coverage from the vertex buffer and cannot run an arbitrary fragment
//! shader, so shader-heavy entries prove themselves through the naga compile above.)

use hl_gl::adapter::glsl;
use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::record;

use hl_gpu::protocol::model::kernel::GlslDescriptor;

// ===================================================================================================
// host route — reproduce `hl-gpu-wgpu::wgsl::glsl_to_wgsl`'s DESKTOP path (naga glsl-in → fixups →
// validate → wgsl-out) byte-for-byte, so the corpus compiles exactly as the executor would. The two
// fixups (`default_bare_returns`, `reorder_functions_topologically`) are copied verbatim from that module
// (they depend only on `naga`); keeping them in sync is asserted by the shared corpus shapes.
// ===================================================================================================
mod host {
    /// Compile desktop GLSL through the executor's exact naga pipeline. `Ok(wgsl)` on success; `Err(msg)`
    /// carries the naga error (the precise gap signal).
    pub fn compile_desktop(
        src: &str,
        stage: naga::ShaderStage,
        entry: &str,
    ) -> Result<String, String> {
        let mut frontend = naga::front::glsl::Frontend::default();
        let mut module = frontend
            .parse(&naga::front::glsl::Options::from(stage), src)
            .map_err(|e| format!("glsl-in: {e:?}"))?;
        if let Some(ep) = module.entry_points.first_mut() {
            ep.name = entry.to_string();
        }
        default_bare_returns(&mut module);
        reorder_functions_topologically(&mut module);
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|e| format!("validate: {e:?}"))?;
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| format!("wgsl-out: {e}"))
    }

    fn default_bare_returns(module: &mut naga::Module) {
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
        for (_h, f) in module.functions.iter_mut() {
            if let Some(ty) = f.result.as_ref().map(|r| r.ty) {
                fix(&mut f.body, &mut f.expressions, ty);
            }
        }
        for ep in module.entry_points.iter_mut() {
            if let Some(ty) = ep.function.result.as_ref().map(|r| r.ty) {
                fix(&mut ep.function.body, &mut ep.function.expressions, ty);
            }
        }
    }

    fn reorder_functions_topologically(module: &mut naga::Module) {
        use naga::{Function, Handle, Span};
        let old = std::mem::take(&mut module.functions);
        let mut owned: Vec<Option<(Function, Span)>> = old
            .iter()
            .map(|(h, f)| Some((f.clone(), old.get_span(h))))
            .collect();
        let n = owned.len();
        let mut callees: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, slot) in owned.iter().enumerate() {
            collect_call_targets(&slot.as_ref().expect("present").0.body, &mut callees[i]);
        }
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut state = vec![0u8; n];
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
            remap_call_targets(&mut f.body, &map);
            remap_call_result_exprs(f, &map);
        }
        for ep in module.entry_points.iter_mut() {
            remap_call_targets(&mut ep.function.body, &map);
            remap_call_result_exprs(&mut ep.function, &map);
        }
        module.functions = new_arena;
    }

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
                Statement::Block(b) => collect_call_targets(b, out),
                Statement::If { accept, reject, .. } => {
                    collect_call_targets(accept, out);
                    collect_call_targets(reject, out);
                }
                Statement::Switch { cases, .. } => {
                    for c in cases {
                        collect_call_targets(&c.body, out);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    collect_call_targets(body, out);
                    collect_call_targets(continuing, out);
                }
                _ => {}
            }
        }
    }

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
                Statement::Block(b) => remap_call_targets(b, map),
                Statement::If { accept, reject, .. } => {
                    remap_call_targets(accept, map);
                    remap_call_targets(reject, map);
                }
                Statement::Switch { cases, .. } => {
                    for c in cases.iter_mut() {
                        remap_call_targets(&mut c.body, map);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    remap_call_targets(body, map);
                    remap_call_targets(continuing, map);
                }
                _ => {}
            }
        }
    }
}

// ===================================================================================================
// corpus scaffolding
// ===================================================================================================

/// Which host route a corpus entry is expected to take.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Path {
    /// The driver's `translate_render` desktopizes it; the emitted GLSL 460 is compiled through
    /// [`host::compile_desktop`] and MUST succeed (this is the driver's own territory).
    Translate,
    /// A `gl_VertexID`/sampler-parameter shape the driver forwards VERBATIM to the executor's ES route.
    /// The battery asserts the routing decision + that the payload is the untranslated source.
    Verbatim,
    /// The driver translates/reflects it correctly, but the EXECUTOR's naga step (`glsl-in`/`wgsl-out`) has
    /// a genuine limitation that no GL-side transform can paper over. The battery drives the shim path (the
    /// driver's own contract), then asserts the host route fails with the documented naga error whose
    /// substring is `.0` — proving the gap is genuinely downstream and is NOT silently passing. These are
    /// reported as known-downstream-gaps, separate from the compile+route coverage number.
    KnownDownstreamGap(&'static str),
}

struct Case {
    name: &'static str,
    vs: &'static str,
    fs: &'static str,
    path: Path,
}

/// Drive the REAL shim path for a vertex+fragment pair and return the linked program's forwarded
/// (vs, fs) GLSL payloads decoded from the `GlslDescriptor` IR words — exactly what the executor receives.
fn link_and_forward(vs: &str, fs: &str) -> (String, String) {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 64,
        height: 64,
    });
    let vso = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vso, vs);
    record::compile_shader(&mut c, vso);
    let fso = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fso, fs);
    record::compile_shader(&mut c, fso);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vso);
    record::attach_shader(&mut c, prog, fso);
    assert!(
        record::link_program(&mut c, prog),
        "glLinkProgram must succeed"
    );
    record::use_program(&mut c, prog);

    let p = c.programs.program(prog).expect("linked program");
    let vw = p.vs_ir.as_ref().expect("vertex payload");
    let fw = p.fs_ir.as_ref().expect("fragment payload");
    let vd = GlslDescriptor::from_words(vw)
        .expect("glsl vs words")
        .expect("decode vs");
    let fd = GlslDescriptor::from_words(fw)
        .expect("glsl fs words")
        .expect("decode fs");
    (vd.source, fd.source)
}

/// Compile a translated (desktop) vertex+fragment pair through the host route, asserting no ES-dialect leak
/// first. `Ok(())` iff BOTH stages compile + validate + emit WGSL.
fn compile_translated(vs_out: &str, fs_out: &str) -> Result<(), String> {
    for (stage_name, src) in [("vertex", vs_out), ("fragment", fs_out)] {
        for banned in [
            "#version 300 es",
            "\nattribute ",
            "\nvarying ",
            "gl_FragColor",
            "texture2D(",
        ] {
            if src.contains(banned) {
                return Err(format!(
                    "{stage_name}: ES dialect token {banned:?} leaked:\n{src}"
                ));
            }
        }
    }
    host::compile_desktop(vs_out, naga::ShaderStage::Vertex, "vmain")
        .map_err(|e| format!("vertex naga: {e}\n---emitted vs---\n{vs_out}"))?;
    host::compile_desktop(fs_out, naga::ShaderStage::Fragment, "fmain")
        .map_err(|e| format!("fragment naga: {e}\n---emitted fs---\n{fs_out}"))?;
    Ok(())
}

/// Run one corpus case: drive the shim path, then compile-or-route per its expected [`Path`].
fn run_case(case: &Case) -> Result<(), String> {
    let (vs_out, fs_out) = link_and_forward(case.vs, case.fs);
    match case.path {
        Path::KnownDownstreamGap(expect) => {
            // The driver's own contract still holds: the program links and forwards a payload. But the host
            // route must FAIL with the documented naga limitation — if it unexpectedly compiles, the gap is
            // stale and the entry should be promoted to `Translate`.
            match compile_translated(&vs_out, &fs_out) {
                Ok(()) => Err(format!(
                    "expected documented downstream gap ({expect}) but the host route now COMPILES it — \
                     promote this entry to Path::Translate"
                )),
                Err(e) if e.contains(expect) => Ok(()),
                Err(e) => Err(format!("downstream gap surfaced a DIFFERENT error (expected {expect:?}): {e}")),
            }
        }
        Path::Translate => compile_translated(&vs_out, &fs_out),
        Path::Verbatim => {
            // The driver forwards the ES source to the executor's ES route (rather than desktopizing via
            // `translate_render`), applying ONLY the documented naga-acceptance injection so `glsl-in` /
            // `validate` accept it: bare default-block uniforms wrapped into a `layout(binding=0)` block and
            // `layout(location=N)` added to bare `in`/`out` attributes/varyings/outputs
            // ([`glsl::prepare_verbatim_program`]). The body + verbatim constructs (gl_VertexID, combined
            // sampler helpers) are carried untouched.
            let routed = glsl::Source::new(case.vs).is_forward_verbatim()
                || glsl::Source::new(case.fs).is_forward_verbatim();
            if !routed {
                return Err("expected verbatim routing but the driver desktopized it".into());
            }
            let combined = glsl::StageSources::new(case.vs, case.fs).uniform_decls();
            let (evs, efs) = glsl::prepare_verbatim_program(case.vs, case.fs, &combined);
            if vs_out != evs || fs_out != efs {
                return Err("verbatim route must forward source with the documented binding/location injection".into());
            }
            Ok(())
        }
    }
}

// ===================================================================================================
// THE CORPUS — ANGLE / GskGpu-shaped GLSL-ES, one entry per real failure mode
// ===================================================================================================
include!("render/corpus.rs");

#[path = "render/glsl.rs"]
mod render;
#[path = "render/struct_equality.rs"]
mod struct_equality;
#[path = "render/struct_uniform.rs"]
mod struct_uniform;
#[path = "render/struct_sampler.rs"]
mod struct_sampler;

#[test]
fn corpus_compiles_and_routes() {
    let cases = corpus();
    let n = cases.len();
    let gaps = cases
        .iter()
        .filter(|c| matches!(c.path, Path::KnownDownstreamGap(_)))
        .count();
    let coverage_total = n - gaps;
    let mut passed = 0usize; // translate-compiles + verbatim-routes (the driver's own coverage)
    let mut gaps_confirmed = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let is_gap = matches!(case.path, Path::KnownDownstreamGap(_));
        match run_case(case) {
            Ok(()) if is_gap => gaps_confirmed += 1,
            Ok(()) => passed += 1,
            Err(e) => failures.push(format!("[{}] {}", case.name, e)),
        }
    }
    eprintln!(
        "GLSL corpus: {passed}/{coverage_total} shaders compile+route on the driver path \
         ({n} total; {gaps_confirmed}/{gaps} known executor-side gaps confirmed downstream)"
    );
    if !failures.is_empty() {
        panic!(
            "{} corpus failures:\n\n{}",
            failures.len(),
            failures.join("\n\n----------\n\n")
        );
    }
}
