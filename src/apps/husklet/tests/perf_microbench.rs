//! RENDER-PATH PERFORMANCE microbenchmark + regression tripwire suite (task #182).
//!
//! This is a PERFORMANCE test, not a correctness test: it measures the hot render-path stages and asserts
//! GENEROUS thresholds whose only job is to trip when something regresses by ~5-10x (a new O(n^2) in the
//! encoder, a per-frame allocation leak, a copy that fell off the fast path). It is NOT a benchmark gate —
//! the absolute numbers are modest and every measured value is PRINTED so a human reading the test log sees
//! the real figures.
//!
//! WHY THE CPU REFERENCE EXECUTOR (not lavapipe/wgpu): the render path under test is the neutral pipeline
//! `encode → decode → validate → account → dispatch → execute → readback`. The reference
//! [`CpuExecutor`] runs that ENTIRE pipeline in-process with no GPU device, no ICD, and no shader
//! compilation, so its timings are DETERMINISTIC and dependency-free — exactly what a CI tripwire wants.
//! (The lavapipe-backed graphics tests exist separately in `tests/common/wgpu.rs`; a software-Vulkan device
//! bring-up is far too noisy to hang a regression threshold on.) The reference executor is also the semantic
//! oracle every real backend is conformance-checked against, so a regression here is a regression everywhere.
//!
//! DEBUG BUILD: this runs as a normal `cargo test` (debug), NOT `--release`. Debug codegen is several times
//! slower than release, so the numbers here are debug numbers and every threshold is sized for a slow debug
//! build on a slow CI box. Running under `--release` will simply beat the thresholds by a wide margin.
//!
//! STRUCTURE IS DETERMINISTIC: every loop uses a FIXED iteration count (never a wall-clock-bounded loop), so
//! the amount of work is identical run-to-run and only the elapsed time varies. Thresholds are ENV-TUNABLE
//! (see [`env_f64`]) for the rare slow box, but the generous defaults should never flake.

use std::time::Instant;

use hl_gpu::protocol::model::descriptor::{BufferDesc, ColorAttachment, TextureDesc};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CommandSink, CpuExecutor, Enc, InProcessCommandSink, TextureId,
};

// ===================================================================================================
// Fixed workload sizes (deterministic — never derived from wall-clock).
// ===================================================================================================

/// Side of the square render target used by the frame/readback benches (256×256 RGBA8 = 256 KiB).
const FRAME_DIM: u32 = 256;
/// "A few hundred draws into one target": fixed-function `ClearRect` ops standing in for draws (they
/// rasterize real pixels on the CPU executor without needing a SPIR-V pipeline, so the frame stays a pure
/// render-path measurement rather than a shader-compile measurement).
const DRAWS_PER_FRAME: usize = 300;
/// Encode+decode round-trips measured in bench #1.
const ENCODE_ITERS: usize = 2_000;
/// Full-frame readbacks measured in bench #3.
const READBACK_ITERS: usize = 500;
/// Frames replayed in the steady-state bench #5 (reusing one resident target — no per-frame create).
const STEADY_FRAMES: usize = 64;
/// Payload size of the "large frame" in bench #4 (16 MiB — comfortably under the CPU executor's 64 MiB
/// per-frame and 256 MiB per-buffer ceilings).
const LARGE_FRAME_BYTES: usize = 16 << 20;

// ===================================================================================================
// Threshold helpers — generous defaults, ENV-overridable for a pathologically slow box.
// ===================================================================================================

/// Read an `f64` threshold from `var`, falling back to `default`. Lets a slow CI box relax a tripwire
/// (`HL_PERF_FRAME_CEIL_MS=500 cargo test ...`) without touching the source.
fn env_f64(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ===================================================================================================
// Representative IR builders.
// ===================================================================================================

/// The render target descriptor: an `Rgba8Unorm` color target usable as a render attachment and as a
/// copy source (so a readback copy can pull it back to a buffer).
fn target_desc(dim: u32) -> TextureDesc {
    TextureDesc {
        width: dim,
        height: dim,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

/// One frame's worth of encoder work: clear the target, then `draws` small `ClearRect` "draws" tiled
/// across it at varying positions/colors (so the work is real per-op raster, not a no-op). `tex` is the
/// render-target resource id. Deterministic: op `i` always lands at the same rect with the same color.
fn frame_command_buffer(tex: u32, dim: u32, draws: usize) -> CommandBuffer {
    let mut encoder = Vec::with_capacity(draws + 2);
    encoder.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: tex,
            load: LoadOp::Clear,
            clear: [0.02, 0.02, 0.05, 1.0],
            store: true,
        }],
        depth: None,
    });
    let rect = 16u32;
    let span = dim.saturating_sub(rect).max(1);
    for i in 0..draws {
        let x = ((i as u32).wrapping_mul(37)) % span;
        let y = ((i as u32).wrapping_mul(53)) % span;
        let c = (i as f32) / (draws.max(1) as f32);
        encoder.push(Enc::ClearRect {
            texture: tex,
            x,
            y,
            w: rect,
            h: rect,
            color: [c, 1.0 - c, 0.5, 1.0],
        });
    }
    encoder.push(Enc::EndRenderPass);
    CommandBuffer {
        encoder,
        signal: None,
    }
}

/// A representative *mixed* command stream (the kind a real frame carries): resource creation, a buffer
/// write, a device-side copy, a full render-pass Submit with many draws, and a present. Used by the
/// encode/decode-throughput bench. Ids are self-consistent so the stream is also a VALID frame.
fn representative_stream(dim: u32, draws: usize) -> Vec<Cmd> {
    const TEX: u32 = 1;
    const SURF: u32 = 2;
    const BUF_A: u32 = 3;
    const BUF_B: u32 = 4;
    let bytes = (dim * dim * 4) as u64;
    vec![
        Cmd::CreateTexture(TEX, target_desc(dim)),
        Cmd::CreateBuffer(
            BUF_A,
            BufferDesc {
                size: bytes,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            BUF_B,
            BufferDesc {
                size: bytes,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: BUF_A,
            offset: 0,
            data: vec![0xABu8; 4096],
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer {
                src: BUF_A,
                src_offset: 0,
                dst: BUF_B,
                dst_offset: 0,
                size: 4096,
            }],
            signal: None,
        }),
        Cmd::Submit(frame_command_buffer(TEX, dim, draws)),
        Cmd::Present {
            surface: SURF,
            texture: TEX,
        },
    ]
}

/// Round `n` down to a 4-byte boundary (the runtime's `copy_alignment`).
fn align4(n: usize) -> usize {
    n & !3
}

// ===================================================================================================
// Bench #1 — IR encode/decode round-trip throughput.
// ===================================================================================================
//
// The wire serializer/deserializer sits on the hot path of every socketed submit. A regression here (an
// accidental per-command reallocation, a decode that rescans) shows up as collapsed round-trips/sec. We
// build one representative stream, prove the round-trip is loss-free once, then time a FIXED number of
// encode+decode cycles.
#[test]
fn ir_encode_decode_throughput() {
    let stream = representative_stream(FRAME_DIM, DRAWS_PER_FRAME);
    let encoded = hl_gpu::Encoder::stream(&stream);
    let frame_bytes = encoded.len();

    // Correctness gate: the round-trip must reproduce the exact stream (a silent lossy encode would make
    // any throughput number meaningless).
    let decoded = hl_gpu::Decoder::stream(&encoded).expect("decode round-trips");
    assert_eq!(decoded, stream, "encode→decode must be loss-free");

    let t0 = Instant::now();
    let mut sink_bytes = 0usize; // consume results so the loop can't be optimized away.
    for _ in 0..ENCODE_ITERS {
        let enc = hl_gpu::Encoder::stream(&stream);
        sink_bytes ^= enc.len();
        let dec = hl_gpu::Decoder::stream(&enc).expect("decode");
        sink_bytes ^= dec.len();
    }
    let elapsed = t0.elapsed();
    assert!(sink_bytes != usize::MAX, "kept the optimizer honest");

    let per_op = elapsed.as_secs_f64() / ENCODE_ITERS as f64;
    let round_trips_per_sec = 1.0 / per_op;
    let mb_per_sec = (frame_bytes as f64 * ENCODE_ITERS as f64) / elapsed.as_secs_f64() / 1e6;
    println!(
        "[perf] ir_encode_decode: {ENCODE_ITERS} round-trips of a {} cmd / {frame_bytes} B frame in {:.3}s \
         => {:.0} round-trips/s, {:.1} µs/op, {:.1} MB/s",
        stream.len(),
        elapsed.as_secs_f64(),
        round_trips_per_sec,
        per_op * 1e6,
        mb_per_sec,
    );

    // Tripwire: total budget for the fixed 2 000 round-trips. Observed debug time is well under a second;
    // the default 15 s ceiling is ~30-50x headroom so only a catastrophic regression trips it.
    let budget_ms = env_f64("HL_PERF_ENCODE_BUDGET_MS", 15_000.0);
    assert!(
        elapsed.as_secs_f64() * 1e3 <= budget_ms,
        "encode/decode of {ENCODE_ITERS} frames took {:.1}ms > {:.1}ms budget",
        elapsed.as_secs_f64() * 1e3,
        budget_ms,
    );
}

// ===================================================================================================
// Bench #2 — executor frame time (submit + complete of one standard scene).
// ===================================================================================================
//
// The CPU executor is synchronous, so "submit + complete" is a single blocking `submit` through the full
// runtime pipeline (validate → account → dispatch → execute). We create the target ONCE, warm up, then
// time one representative frame (clear + 300 raster ops into one 256×256 target).
#[test]
fn executor_frame_time() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    sink.submit(&[Cmd::CreateTexture(1, target_desc(FRAME_DIM))])
        .expect("create target");

    let frame = vec![Cmd::Submit(frame_command_buffer(
        1,
        FRAME_DIM,
        DRAWS_PER_FRAME,
    ))];

    // Warm up the allocator / caches so the measured frame reflects steady state, not first-touch.
    for _ in 0..3 {
        sink.submit(&frame).expect("warmup frame");
    }

    // Average a small fixed number of frames to smooth scheduler jitter (still deterministic in work).
    const MEASURED: usize = 10;
    let t0 = Instant::now();
    for _ in 0..MEASURED {
        sink.submit(&frame).expect("measured frame");
    }
    let per_frame_ms = t0.elapsed().as_secs_f64() * 1e3 / MEASURED as f64;

    println!(
        "[perf] executor_frame_time: {}×{} target, {DRAWS_PER_FRAME} raster ops/frame => {:.3} ms/frame",
        FRAME_DIM, FRAME_DIM, per_frame_ms,
    );

    // Tripwire: a 256×256 frame with 300 raster ops is a fraction of a ms of real work; 60 ms/frame is
    // enormous headroom for a slow debug box, yet still catches a 5-10x per-frame blowup.
    let ceil_ms = env_f64("HL_PERF_FRAME_CEIL_MS", 60.0);
    assert!(
        per_frame_ms <= ceil_ms,
        "frame time {:.3}ms > {:.3}ms ceiling",
        per_frame_ms,
        ceil_ms
    );
}

// ===================================================================================================
// Bench #3 — device→host full-frame readback latency.
// ===================================================================================================
//
// Reading a rendered target back to host memory is its own hot path (a real app polls it every frame). We
// render one frame, then time a FIXED number of full-frame `read_texture` calls (256 KiB each).
#[test]
fn readback_latency() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    sink.submit(&[Cmd::CreateTexture(1, target_desc(FRAME_DIM))])
        .expect("create target");
    sink.submit(&[Cmd::Submit(frame_command_buffer(
        1,
        FRAME_DIM,
        DRAWS_PER_FRAME,
    ))])
    .expect("render");

    let frame_bytes = (FRAME_DIM * FRAME_DIM * 4) as usize;
    let mut out = vec![0u8; frame_bytes];

    // Warm up.
    sink.executor()
        .read_texture(sink.resources(), TextureId(1), &mut out)
        .expect("warmup readback");

    let t0 = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..READBACK_ITERS {
        sink.executor()
            .read_texture(sink.resources(), TextureId(1), &mut out)
            .expect("readback");
        checksum = checksum.wrapping_add(out[0] as u64); // consume so it can't be elided.
    }
    let elapsed = t0.elapsed();
    assert!(checksum != u64::MAX, "kept the optimizer honest");

    let per_readback_ms = elapsed.as_secs_f64() * 1e3 / READBACK_ITERS as f64;
    let gb_per_sec = (frame_bytes as f64 * READBACK_ITERS as f64) / elapsed.as_secs_f64() / 1e9;
    println!(
        "[perf] readback_latency: {READBACK_ITERS}× full-frame ({frame_bytes} B) readback => {:.4} ms/readback, {:.2} GB/s",
        per_readback_ms, gb_per_sec,
    );

    // Tripwire: a 256 KiB memcpy-class readback is tens of microseconds; 20 ms/readback is vast headroom
    // but still trips a 100x+ regression (e.g. a readback that started re-decoding the whole frame).
    let ceil_ms = env_f64("HL_PERF_READBACK_CEIL_MS", 20.0);
    assert!(
        per_readback_ms <= ceil_ms,
        "readback {:.4}ms > {:.4}ms ceiling",
        per_readback_ms,
        ceil_ms
    );
}

// ===================================================================================================
// Bench #4 — large-frame transport round-trip throughput.
// ===================================================================================================
//
// A big frame (a 16 MiB buffer upload + a device copy) is round-tripped through the command sink the way a
// socketed submit would be: encode → decode → submit (validate/account/dispatch/execute) → read back and
// verify. Throughput is reported in MB/s over the encoded frame size. This catches a regression that makes
// large-payload handling super-linear (an extra copy of the payload, a byte-at-a-time validate).
#[test]
fn large_frame_transport() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());

    let payload_len = align4(LARGE_FRAME_BYTES);
    let buf_size = payload_len as u64;
    // Distinct byte pattern so the readback verify is meaningful.
    let payload: Vec<u8> = (0..payload_len).map(|i| (i as u8) ^ 0x5A).collect();

    sink.submit(&[
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: buf_size,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            2,
            BufferDesc {
                size: buf_size,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
    ])
    .expect("create large buffers");

    // The large frame: upload the payload, then a device-side copy of the whole thing to a second buffer.
    let frame = vec![
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: payload.clone(),
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer {
                src: 1,
                src_offset: 0,
                dst: 2,
                dst_offset: 0,
                size: buf_size,
            }],
            signal: None,
        }),
    ];

    // Fixed small iteration count — the payload is large, so a few round-trips is plenty of signal.
    const MEASURED: usize = 5;
    let encoded_once = hl_gpu::Encoder::stream(&frame);
    let frame_bytes = encoded_once.len();

    let t0 = Instant::now();
    for _ in 0..MEASURED {
        // Full transport shape: serialize, deserialize, then run it through the sink.
        let enc = hl_gpu::Encoder::stream(&frame);
        let dec = hl_gpu::Decoder::stream(&enc).expect("decode large frame");
        sink.submit(&dec).expect("submit large frame");
    }
    let elapsed = t0.elapsed();

    // Verify the last round-trip actually landed the bytes (transport must be lossless).
    let back = sink
        .read_buffer(BufferId(2), 0, payload_len)
        .expect("read back dst");
    assert_eq!(back, payload, "large-frame transport must be byte-exact");

    let total_bytes = frame_bytes as f64 * MEASURED as f64;
    let mb_per_sec = total_bytes / elapsed.as_secs_f64() / 1e6;
    println!(
        "[perf] large_frame_transport: {MEASURED}× {:.1} MiB frame (encode+decode+submit+exec) in {:.3}s => {:.1} MB/s",
        frame_bytes as f64 / (1 << 20) as f64,
        elapsed.as_secs_f64(),
        mb_per_sec,
    );

    // Tripwire: a generous FLOOR. Observed debug throughput is comfortably in the hundreds of MB/s; a
    // 40 MB/s floor is ~5-10x below that, so it only trips if large-payload handling went super-linear.
    let floor_mbps = env_f64("HL_PERF_TRANSPORT_FLOOR_MBPS", 40.0);
    assert!(
        mb_per_sec >= floor_mbps,
        "transport {:.1} MB/s < {:.1} MB/s floor",
        mb_per_sec,
        floor_mbps
    );
}

// ===================================================================================================
// Bench #5 — repeated-frame steady state (no per-frame time blowup).
// ===================================================================================================
//
// Replay the SAME frame K times against one resident target and one long-lived session. If per-frame work
// is bounded (no leak, no quadratic growth in the session's resource tables or the executor), the last
// frame should cost about the same as the first. We assert the last frame is within a generous FACTOR of
// the median of the first few frames — a tripwire for per-frame growth, tolerant of one-off jitter.
#[test]
fn repeated_frame_steady_state() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    sink.submit(&[Cmd::CreateTexture(1, target_desc(FRAME_DIM))])
        .expect("create target");
    let frame = vec![Cmd::Submit(frame_command_buffer(
        1,
        FRAME_DIM,
        DRAWS_PER_FRAME,
    ))];

    let mut per_frame_us = Vec::with_capacity(STEADY_FRAMES);
    for _ in 0..STEADY_FRAMES {
        let t = Instant::now();
        sink.submit(&frame).expect("steady frame");
        per_frame_us.push(t.elapsed().as_secs_f64() * 1e6);
    }

    // Baseline = median of the first 8 frames (robust against a cold first-frame spike). Compare the
    // median of the LAST 8 frames against it so a single jittery sample doesn't dominate either end.
    let median = |slice: &[f64]| {
        let mut v = slice.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let head = median(&per_frame_us[..8]);
    let tail = median(&per_frame_us[STEADY_FRAMES - 8..]);
    let first = per_frame_us[0];
    let last = *per_frame_us.last().unwrap();
    let max = per_frame_us.iter().cloned().fold(0.0f64, f64::max);
    let min = per_frame_us.iter().cloned().fold(f64::MAX, f64::min);
    let growth = tail / head.max(1e-9);

    println!(
        "[perf] repeated_frame_steady_state: {STEADY_FRAMES} frames — first {:.1}µs last {:.1}µs \
         (head-median {:.1}µs, tail-median {:.1}µs, min {:.1}µs, max {:.1}µs) => {:.2}x tail/head",
        first, last, head, tail, min, max, growth,
    );

    // Tripwire: the tail should not balloon relative to the head. A 6x factor tolerates real scheduler
    // jitter on a shared CI box while still catching a genuine per-frame leak / quadratic growth (which
    // shows up as a steadily climbing tail, not a one-off spike).
    let factor = env_f64("HL_PERF_STEADY_FACTOR", 6.0);
    assert!(
        growth <= factor,
        "steady-state per-frame time grew {:.2}x (head {:.1}µs → tail {:.1}µs) > {:.1}x allowed",
        growth,
        head,
        tail,
        factor,
    );
}
