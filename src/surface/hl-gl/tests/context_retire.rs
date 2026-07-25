//! Context-teardown residency retirement — the fix for Chrome's lost-context death spiral.
//!
//! Chrome (and Skia/GskGpu) frees its GL objects by DESTROYING THE CONTEXT, never by
//! `glDeleteTexture`/`glDeleteProgram`. When it loses a context and recreates its entire working set with
//! FRESH GL names every cycle, the shim used to keep every abandoned cycle's IR resources resident on the
//! host, climbing the per-connection residency ledger until every swap NACKed `connection residency`.
//!
//! [`GlContext::retire_all`] fixes it: at context teardown it queues a `Destroy*` for every resident IR
//! resource so the next submitted frame refunds the whole working set. These tests prove it two ways:
//!   * MODEL: after a working set is built, `retire_all` empties every residency cache and queues a
//!     `Destroy*` for each resident id.
//!   * END-TO-END: driven through the real runtime accounting pipeline (`InProcessCommandSink` over the
//!     reference `CpuExecutor`), the per-connection residency ledger returns to ZERO after teardown.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::capability::shader_payload;
use hl_gpu::{
    Cmd, CpuExecutor, FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink, Limits, Session,
};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str =
    "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

/// A GLES-accepting in-process sink over the CPU reference executor with a real per-connection ledger.
fn cpu_sink() -> InProcessCommandSink<CpuExecutor> {
    let exec = CpuExecutor::new();
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    limits.caps.shader_payloads |= shader_payload::MSL | shader_payload::GLSL;
    let session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    InProcessCommandSink::with_session(session, exec)
}

/// A `24`-byte position+color vertex the CPU rasterizer reads.
fn vertex(pos: [f32; 2]) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    for f in pos.iter().chain([1.0f32, 0.0, 0.0, 1.0].iter()) {
        v.extend_from_slice(&f.to_le_bytes());
    }
    v
}

/// Record a full working set into `c`: a flat program (2 shaders + a pipeline), a vertex buffer, and a
/// clear + triangle draw targeting the default framebuffer (which mints the default render-target texture +
/// presentable surface). Enough distinct resource KINDS that "retire everything" is a real claim.
fn record_working_set(c: &mut GlContext) {
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let mut verts = Vec::new();
    for p in [[-0.8f32, -0.8], [0.8, -0.8], [0.0, 0.8]] {
        verts.extend(vertex(p));
    }
    record::buffer_data(c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 24, 0);
    record::enable_vertex_attrib(c, 0);

    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);

    record::clear_color(c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(c);
    record::draw_arrays(c, GL_TRIANGLES, 0, 3);
}

// ---------------------------------------------------------------------------------------------------
// MODEL: retire_all queues a Destroy* for every resident IR resource and empties every cache
// ---------------------------------------------------------------------------------------------------

#[test]
fn retire_all_queues_destroy_for_the_whole_working_set() {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 16,
        height: 16,
    };
    let mut sink = hl_gpu::RecordingSink::with_full_caps();

    record_working_set(&mut c);
    // Present the frame so the default target + program shaders/pipeline + vbo are materialized as resident
    // IR (their `Create*` recorded into the caches).
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(
        !c.has_pending_destroys(),
        "a healthy frame queues no persistent destroys"
    );

    // Context teardown: retire the whole working set.
    c.retire_all();

    let pending = c.pending_destroys();
    assert!(
        !pending.is_empty(),
        "retire_all must queue destroys for the resident working set"
    );

    // Every queued command is a Destroy*, and it covers at least one of each resource kind we created
    // (buffer, two shaders, a pipeline, and the default target's texture + surface).
    assert!(
        pending.iter().all(is_destroy),
        "retire_all queues only Destroy* commands"
    );
    assert!(
        pending.iter().any(|c| matches!(c, Cmd::DestroyBuffer(_))),
        "vbo retired"
    );
    assert_eq!(
        pending
            .iter()
            .filter(|c| matches!(c, Cmd::DestroyShader(_)))
            .count(),
        2,
        "both program shader modules retired"
    );
    assert!(
        pending.iter().any(|c| matches!(c, Cmd::DestroyPipeline(_))),
        "pipeline retired"
    );
    assert!(
        pending.iter().any(|c| matches!(c, Cmd::DestroyTexture(_))),
        "default target texture retired"
    );
    assert!(
        pending.iter().any(|c| matches!(c, Cmd::DestroySurface(_))),
        "default surface retired"
    );

    // A second retire is a clean no-op: the caches are already empty.
    let n = c.pending_destroys().len();
    c.retire_all();
    assert_eq!(
        c.pending_destroys().len(),
        n,
        "a second retire_all adds nothing (caches already empty)"
    );
}

fn is_destroy(c: &Cmd) -> bool {
    matches!(
        c,
        Cmd::DestroyBuffer(_)
            | Cmd::DestroyTexture(_)
            | Cmd::DestroySampler(_)
            | Cmd::DestroyShader(_)
            | Cmd::DestroyPipeline(_)
            | Cmd::DestroyBindGroup(_)
            | Cmd::DestroySurface(_)
            | Cmd::DestroyFence(_)
    )
}

// ---------------------------------------------------------------------------------------------------
// END-TO-END: teardown refunds the connection residency ledger back to ZERO
// ---------------------------------------------------------------------------------------------------

#[test]
fn context_teardown_refunds_the_whole_residency_ledger() {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 16,
        height: 16,
    };
    let mut sink = cpu_sink();

    // Baseline: nothing resident.
    assert_eq!(sink.session().residency_bytes(), 0);
    assert_eq!(sink.session().object_count(), 0);

    // Build + present a working set through the accounting pipeline: residency climbs.
    record_working_set(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let resident = sink.session().residency_bytes();
    let objects = sink.session().object_count();
    assert!(
        resident > 0 && objects > 0,
        "a presented frame charges residency ({resident} B, {objects} obj)"
    );

    // Context teardown (eglDestroyContext of the last live context): retire the whole working set, then let
    // the next swap flush the queued Destroy* standalone (no draws pending → the "delete-only frame" path).
    c.retire_all();
    assert!(
        !swap::swap_buffers(&mut c, &mut sink).unwrap(),
        "delete-only frame presents nothing"
    );

    // The ledger is back to baseline: the abandoned working set no longer holds ANY host residency.
    assert_eq!(
        sink.session().residency_bytes(),
        0,
        "teardown refunds every resident byte"
    );
    assert_eq!(
        sink.session().object_count(),
        0,
        "teardown refunds every resident object"
    );
}

// ---------------------------------------------------------------------------------------------------
// LOST-CONTEXT LOOP: recreating the working set across cycles stays BOUNDED (no per-cycle accumulation)
// ---------------------------------------------------------------------------------------------------

#[test]
fn repeated_recreate_teardown_cycles_do_not_accumulate() {
    let mut sink = cpu_sink();
    let mut high_water = 0u64;

    // Mimic Chrome's death spiral: each cycle recreates the entire working set with FRESH GL names (a fresh
    // GlContext, exactly what a lost-then-recreated context does) and tears the prior one down. Without
    // retirement the ledger would climb one working set per cycle; with it, it returns to zero every cycle.
    for cycle in 0..8 {
        let mut c = GlContext::new();
        c.surf = GlSurface {
            have: true,
            width: 16,
            height: 16,
        };
        record_working_set(&mut c);
        assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
        high_water = high_water.max(sink.session().residency_bytes());

        // Teardown of this cycle's context refunds its residency before the next cycle.
        c.retire_all();
        assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
        assert_eq!(
            sink.session().residency_bytes(),
            0,
            "cycle {cycle}: ledger returns to zero after teardown (no accumulation)"
        );
    }

    // The ledger never exceeded a SINGLE working set's footprint across all cycles — bounded, not climbing.
    assert!(high_water > 0);
    assert_eq!(sink.session().residency_bytes(), 0);
}
