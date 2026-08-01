use super::*;
use super::runners::read_plane;

// =================================================================================================
// the differential test
// =================================================================================================
use super::msaa::analytic_msaa_resolve;
use super::runners::{diff, run_cpu, run_wgpu};

#[test]
fn differential_cpu_oracle_vs_wgpu() {
    // 24 generators × 10 seeds each — every generator gets 10 seeds.
    const N: u64 = 240;

    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let mut agreed = 0u32;
    let mut per_category: std::collections::BTreeMap<&'static str, (u32, u32)> = Default::default(); // (agreed, total)
    let mut ops_covered: BTreeSet<&'static str> = BTreeSet::new();
    let mut divergences: Vec<String> = Vec::new();

    for i in 0..N {
        let gen = GENERATORS[(i as usize) % GENERATORS.len()];
        let seed = i; // pure index seeding
        let prog = gen(seed);
        for op in &prog.ops {
            ops_covered.insert(op);
        }
        let entry = per_category.entry(prog.category).or_insert((0, 0));
        entry.1 += 1;

        let cpu_out = run_cpu(&prog);
        let gpu_out = run_wgpu(&mut exec, &prog);

        match (cpu_out, gpu_out) {
            (Ok(c), Ok(g)) => match diff(&c, &g, prog.tol, read_plane(&prog)) {
                None => {
                    agreed += 1;
                    entry.0 += 1;
                }
                Some(desc) => divergences.push(format!(
                    "DIVERGENCE [{}] seed={} ({} bytes): {}",
                    prog.category,
                    prog.seed,
                    c.len(),
                    desc
                )),
            },
            (Err(ce), Ok(_)) => divergences.push(format!(
                "DIVERGENCE [{}] seed={}: CPU oracle errored ({ce:?}) but wgpu ran",
                prog.category, prog.seed
            )),
            (Ok(_), Err(ge)) => divergences.push(format!(
                "DIVERGENCE [{}] seed={}: wgpu errored ({ge:?}) but CPU oracle ran",
                prog.category, prog.seed
            )),
            (Err(ce), Err(ge)) => {
                // Both refused the program. If they refused with the same error kind that is agreement (not
                // a pixel divergence); a different error kind is itself a divergence worth reporting.
                if std::mem::discriminant(&ce) == std::mem::discriminant(&ge) {
                    agreed += 1;
                    entry.0 += 1;
                } else {
                    divergences.push(format!(
                        "DIVERGENCE [{}] seed={}: both errored but differently: cpu={ce:?} gpu={ge:?}",
                        prog.category, prog.seed
                    ));
                }
            }
        }
    }

    // ---- ANALYTIC (executor-vs-hand-computed) MSAA-resolve checks — NOT oracle-compared ----------
    let (msaa_checks, msaa_fails) = analytic_msaa_resolve(&mut exec);

    // ---- summary -------------------------------------------------------------------------------
    let oracle_compared = [
        "clear / clear_rect / copies / fill_buffer — EXACT byte moves",
        "draw_flat / draw_gradient / draw_depth / draw_blend — fixed-function raster (±1/±2)",
        "blit_nearest (EXACT) / blit_linear (±3 bilinear)",
        "compute_iota — neutral kernel-IR on both backends (EXACT)",
        "compute_fcmp — ordered f32 compare on NEGATIVE operands via Inst::FSetp; a signed-integer compare \
         of the float bit patterns inverts here (EXACT)",
        "clear_srgb / draw_srgb — sRGB gamma-encode on write, oracle now matches the ROP (±2)",
        "stencil_equal / stencil_greater — two-pass mark-then-test, oracle now models stencil (EXACT)",
        "draw_mask_rgb / draw_mask_alpha — per-channel write_mask, oracle now honors ColorTargetState.write_mask \
         (masked channels keep the clear, written channels are a flat replace of an exact k/255 constant — EXACT)",
        "cull_ccw_back / cull_ccw_front / cull_cw_front / cull_rev_ccw_front — face culling over triangles of \
         known winding under both front_face conventions + both cull faces, oracle now honors \
         RenderPipelineDesc.cull/front_face (culled → the exact bg clear; drawn → the exact fg replace — EXACT)",
    ];
    let analytic_only = [
        "MSAA + ResolveTexture — the oracle has no multisample-RENDER concept (validate rejects a \
         sample_count>1 attachment), so the executor's 4× render+resolve is asserted against a HAND-COMPUTED \
         analytic expectation (full-coverage exact colour; half-coverage exact fg/bg + averaged edge), NOT \
         against the oracle. Counted separately below.",
    ];
    let remaining_exclusions = [
        "NONE for the op surface: stencil, sRGB, write_mask, face-culling, and MSAA-resolve are all now \
         covered (stencil + sRGB + write_mask + cull oracle-compared; MSAA-resolve analytically checked). \
         DrawIndexed stays out of the per-pixel fuzz only to avoid partial-coverage indexed edge-rule \
         ambiguity (exercised by the coverage suite); the non-base-mip CopyTextureToBuffer readback stays a \
         documented reject on both backends (the oracle stores only the base mip plane).",
    ];
    println!("======================== DIFFERENTIAL SUMMARY ========================");
    println!(
        "oracle-compared programs: {N}   agreed: {agreed}   divergences: {}",
        divergences.len()
    );
    println!(
        "analytic (executor-vs-hand-computed) MSAA checks: {msaa_checks}   failures: {}",
        msaa_fails.len()
    );
    println!("per-category (agreed/total):");
    for (cat, (a, t)) in &per_category {
        println!("    {cat:<16} {a}/{t}");
    }
    println!(
        "encoder ops covered ({}): {:?}",
        ops_covered.len(),
        ops_covered
    );
    println!("oracle-compared op families:");
    for e in &oracle_compared {
        println!("    - {e}");
    }
    println!("analytic-only (executor-vs-hand-computed):");
    for e in &analytic_only {
        println!("    - {e}");
    }
    println!("remaining exclusions:");
    for e in &remaining_exclusions {
        println!("    - {e}");
    }
    for d in &divergences {
        println!("  {d}");
    }
    for f in &msaa_fails {
        println!("  ANALYTIC-MSAA FAILURE: {f}");
    }
    println!("======================================================================");

    assert!(
        divergences.is_empty(),
        "the CPU oracle and the wgpu executor diverged on {} of {N} programs:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    assert_eq!(
        agreed, N as u32,
        "every program must agree across both backends"
    );
    assert!(
        msaa_fails.is_empty(),
        "the executor's MSAA render+resolve diverged from the hand-computed analytic expectation:\n{}",
        msaa_fails.join("\n")
    );
    assert!(
        msaa_checks >= 3,
        "expected the full + half + 1×-control MSAA checks to run, got {msaa_checks}"
    );
    // Guard against an accidental empty/broken generator table hiding real coverage.
    assert!(
        ops_covered.len() >= 15,
        "expected broad op coverage, got {}",
        ops_covered.len()
    );
    // The newly-covered op surface must actually be present in the run.
    {
        let op = "SetStencilReference";
        assert!(
            ops_covered.contains(op),
            "expected `{op}` in the covered op set"
        );
    }
    for cat in [
        "clear_srgb",
        "draw_srgb",
        "stencil_equal",
        "stencil_greater",
        "draw_mask_rgb",
        "draw_mask_alpha",
        "cull_ccw_back",
        "cull_ccw_front",
        "cull_cw_front",
        "cull_rev_ccw_front",
    ] {
        assert!(
            per_category.contains_key(cat),
            "expected category `{cat}` to run"
        );
    }
}
