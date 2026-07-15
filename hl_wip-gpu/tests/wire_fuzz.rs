//! Deeper malformed-wire battery for the protocol codec + the decode→runtime pipeline. This COMPLEMENTS
//! `tests/wire_adversarial.rs` (per-op/-command truncation, byte-stability, typed rejection) and the
//! executor hostile-IR sweep in `tests/executor_adversarial.rs` (id-lifecycle + dispatch-clamp driven
//! directly on `Cmd`). It adds three things neither of those cover:
//!
//! 1. **Provable bounded allocation** — a thread-local tracking allocator MEASURES the peak heap growth
//!    while decoding a decode-bomb (a count/length field claiming ~4 billion entries with no backing
//!    bytes). Every count field in the codec (`words`, bind-group entries, vertex attrs, vertex buffers,
//!    color targets, render-pass color attachments, submit encoder length, string labels, and the
//!    capability-handshake name) is asserted to grow the heap by < 16 MiB, so a future edit that swaps a
//!    bounded `cap_count(..)` reservation for a raw `Vec::with_capacity(attacker_count)` fails HERE
//!    instead of OOM-aborting a host. A self-check first proves the allocator actually sees a deliberate
//!    64 MiB allocation, so the bound is not vacuously satisfied.
//! 2. **Capability-handshake robustness** — the handshake decoder (`Capabilities::from_handshake`) is
//!    never fuzzed elsewhere: truncate a valid handshake at every prefix, bit-flip/randomize it, and feed
//!    a name decode-bomb — all must return cleanly / never panic. Plus a too-old / too-new / guest-newer
//!    `wire_version` yields a typed `Unsupported` version error at negotiation.
//! 3. **End-to-end bytes → decode_stream → runtime::submit** — hostile byte streams driven through the
//!    WHOLE pipeline (decode → validate → account → dispatch over the CPU oracle) surface a typed error
//!    (`DuplicateId` / `UnknownId` / `ResourceLimit`) and NEVER panic across many seeds. The existing
//!    fuzz corpora stop at `decode_stream`; this proves the runtime survives every decodable abuse too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use hl_gpu::protocol::codec::wire::Encoder;
use hl_gpu::protocol::model::command::{etag, tag};
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::{
    decode_stream, encode_stream, Capabilities, Cmd, CpuExecutor, FakeClock, FeatureRequest,
    GlobalLedger, GpuError, GpuExecutor, Limits, Result, Session, WIRE_VERSION,
};

// ---------------------------------------------------------------------------------------------------
// thread-local tracking allocator: measures the PEAK live-heap growth of a scoped operation, isolated
// per test thread so parallel tests never perturb the measurement.
// ---------------------------------------------------------------------------------------------------

thread_local! {
    static CUR: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static TRACK: Cell<bool> = const { Cell::new(false) };
}

struct TrackingAlloc;

// SAFETY: forwards every request to the system allocator unchanged; the tracking is a side counter that
// only reads/writes const-initialized thread-locals (never allocates), and `try_with` tolerates TLS
// teardown so no allocation can panic or recurse into the allocator.
unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            let _ = TRACK.try_with(|t| {
                if t.get() {
                    let _ = CUR.try_with(|cur| {
                        let n = cur.get().saturating_add(layout.size());
                        cur.set(n);
                        let _ = PEAK.try_with(|pk| {
                            if n > pk.get() {
                                pk.set(n);
                            }
                        });
                    });
                }
            });
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        let _ = TRACK.try_with(|t| {
            if t.get() {
                let _ = CUR.try_with(|cur| cur.set(cur.get().saturating_sub(layout.size())));
            }
        });
    }
}

#[global_allocator]
static ALLOC: TrackingAlloc = TrackingAlloc;

/// Run `f` while tracking heap allocations on this thread, returning `(f's value, peak live-heap growth
/// above the baseline at entry)`. A transient `Vec::with_capacity` inside `f` is captured in the peak even
/// if it is freed again before `f` returns (e.g. dropped on an error unwind).
fn peak_growth_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CUR.with(|c| c.get());
    PEAK.with(|p| p.set(base));
    TRACK.with(|t| t.set(true));
    let out = f();
    TRACK.with(|t| t.set(false));
    let peak = PEAK.with(|p| p.get());
    (out, peak.saturating_sub(base))
}

/// A decode-bomb is safe when its heap growth is a few KiB (output vec + error context), never the many
/// GiB an unbounded `Vec::with_capacity(attacker_count)` would reserve.
const BOMB_ALLOC_CEIL: usize = 16 << 20; // 16 MiB — orders of magnitude below any unbounded prealloc.

/// A count/length field value that would reserve ~34 GiB (× element size) if it were ever preallocated raw.
const BOMB: u32 = 0xFFFF_FFF0;

// ---------------------------------------------------------------------------------------------------
// decode-bomb frames: each reaches ONE count/length field, sets it to ~4 billion, then stops — so the
// decoder must bound its reservation to the (zero) remaining bytes and fail at the first missing element.
// ---------------------------------------------------------------------------------------------------

/// Top-level `WriteBuffer` data length (`Decoder::bytes` → `take`).
fn write_buffer_data_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::WRITE_BUFFER);
    e.u32(1); // id
    e.u64(0); // offset
    e.u32(BOMB); // data length; nothing follows
    e.into_vec()
}

/// `CreateShader` word count (`Decoder::words`).
fn create_shader_words_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_SHADER);
    e.u32(1); // id
    e.u32(BOMB); // word count; nothing follows
    e.into_vec()
}

/// `Submit` command-buffer encoder length (`dec_command_buffer`, `cap_count(n, 1)`).
fn submit_encoder_len_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::SUBMIT);
    e.u32(BOMB); // encoder op count; nothing follows
    e.into_vec()
}

/// `BeginRenderPass` color-attachment count (`dec_enc`, `cap_count(n, 25)`).
fn begin_render_pass_color_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::SUBMIT);
    e.u32(1); // one encoder op
    e.u8(etag::BEGIN_RENDER_PASS);
    e.u32(BOMB); // color attachment count; nothing follows
    e.into_vec()
}

/// `CreateBindGroup` entry count (`dec_bind_group`, `cap_count(n, 9)`).
fn bind_group_entries_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_BIND_GROUP);
    e.u32(1); // id
    e.u32(0); // set
    e.u32(BOMB); // entry count; nothing follows
    e.into_vec()
}

/// `CreateRenderPipeline` vertex-buffer count (`dec_render_pipeline`, `cap_count(nvb, 12)`).
fn render_pipeline_vertex_buffers_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_RENDER_PIPELINE);
    e.u32(1); // id
    e.u32(0); // vertex.module
    e.str(""); // vertex.entry
    e.bool(false); // no fragment
    e.u32(BOMB); // vertex_buffers count; nothing follows
    e.into_vec()
}

/// `CreateRenderPipeline` color-target count (`dec_render_pipeline`, `cap_count(nct, 9)`).
fn render_pipeline_color_targets_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_RENDER_PIPELINE);
    e.u32(1); // id
    e.u32(0); // vertex.module
    e.str(""); // vertex.entry
    e.bool(false); // no fragment
    e.u32(0); // vertex_buffers = 0
    e.u32(BOMB); // color_targets count; nothing follows
    e.into_vec()
}

/// `VertexLayout` attribute count nested inside a render pipeline (`dec_vertex_layout`, `cap_count(n, 12)`).
fn vertex_layout_attrs_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_RENDER_PIPELINE);
    e.u32(1); // id
    e.u32(0); // vertex.module
    e.str(""); // vertex.entry
    e.bool(false); // no fragment
    e.u32(1); // vertex_buffers = 1
    e.u32(16); // layout.stride
    e.u32(0); // layout.step_mode
    e.u32(BOMB); // attr count; nothing follows
    e.into_vec()
}

/// A length-prefixed string label (`Decoder::str` → `bytes` → `take`) — inherently prealloc-free, asserted.
fn buffer_label_str_bomb() -> Vec<u8> {
    let mut e = Encoder::new();
    e.u8(tag::CREATE_BUFFER);
    e.u32(1); // id
    e.u64(64); // size
    e.u32(0); // usage
    e.u32(BOMB); // label length; nothing follows
    e.into_vec()
}

fn command_bombs() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("write_buffer_data", write_buffer_data_bomb()),
        ("create_shader_words", create_shader_words_bomb()),
        ("submit_encoder_len", submit_encoder_len_bomb()),
        ("begin_render_pass_color", begin_render_pass_color_bomb()),
        ("bind_group_entries", bind_group_entries_bomb()),
        ("render_pipeline_vertex_buffers", render_pipeline_vertex_buffers_bomb()),
        ("render_pipeline_color_targets", render_pipeline_color_targets_bomb()),
        ("vertex_layout_attrs", vertex_layout_attrs_bomb()),
        ("buffer_label_str", buffer_label_str_bomb()),
    ]
}

// ---------------------------------------------------------------------------------------------------
// 1. decode-bombs are typed errors AND provably do not preallocate.
// ---------------------------------------------------------------------------------------------------

#[test]
fn tracking_allocator_detects_a_real_large_allocation() {
    // Integrity guard: prove the tracking allocator actually observes a deliberate 64 MiB allocation, so
    // the bounded-growth assertions below cannot pass vacuously (e.g. if TLS tracking silently no-oped).
    let (_, growth) = peak_growth_during(|| {
        let v = vec![0u8; 64 << 20];
        std::hint::black_box(v.len())
    });
    assert!(growth >= 64 << 20, "allocator failed to observe a 64 MiB allocation (growth={growth})");
}

#[test]
fn every_count_field_is_a_bounded_error_not_a_decode_bomb() {
    for (name, bytes) in command_bombs() {
        // The bytes must be tiny so cloning/setup contributes nothing to the measured growth.
        assert!(bytes.len() < 64, "{name}: bomb frame should be tiny, got {} bytes", bytes.len());
        let (result, growth) = peak_growth_during(|| decode_stream(&bytes));
        assert!(
            result.is_err(),
            "{name}: a count claiming ~4 billion entries with no backing bytes must error, got Ok"
        );
        assert!(
            growth < BOMB_ALLOC_CEIL,
            "{name}: decoding grew the heap by {growth} bytes (>= {BOMB_ALLOC_CEIL}) — a decode-bomb \
             prealloc regression (a bounded cap_count reservation was likely replaced by a raw \
             Vec::with_capacity(attacker_count))"
        );
    }
}

#[test]
fn handshake_name_length_is_a_bounded_error_not_a_decode_bomb() {
    // A handshake body whose name string claims ~4 billion bytes with none following.
    let mut body = Encoder::new();
    body.u32(WIRE_VERSION);
    body.u32(BOMB); // name str length; nothing follows
    let mut e = Encoder::new();
    e.bytes(&body.into_vec()); // wrap as a (u32 length + body) handshake frame
    let bytes = e.into_vec();

    let (result, growth) = peak_growth_during(|| Capabilities::from_handshake(&bytes));
    assert!(result.is_err(), "a handshake name-length bomb must error, got Ok");
    assert!(
        growth < BOMB_ALLOC_CEIL,
        "handshake name decode grew the heap by {growth} bytes (>= {BOMB_ALLOC_CEIL}) — decode-bomb regression"
    );
}

// ---------------------------------------------------------------------------------------------------
// 2. capability-handshake robustness (truncation, fuzz, version mismatch)
// ---------------------------------------------------------------------------------------------------

/// Reproducible LCG byte source (index-derived, no rng/clock).
fn lcg(state: &mut u64) -> u8 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 56) as u8
}

fn no_panic_handshake(bytes: &[u8]) {
    let owned = bytes.to_vec();
    let r = catch_unwind(move || {
        let _ = Capabilities::from_handshake(&owned);
    });
    assert!(r.is_ok(), "from_handshake PANICKED on {} bytes: {:02x?}", bytes.len(), bytes);
}

#[test]
fn handshake_truncated_at_every_prefix_never_panics() {
    let good = Capabilities::full("truncate-me").to_handshake();
    // The intact handshake decodes to exactly the source caps.
    assert_eq!(Capabilities::from_handshake(&good).unwrap(), Capabilities::full("truncate-me"));
    for cut in 0..good.len() {
        no_panic_handshake(&good[..cut]); // Err is fine; a panic/hang is not.
    }
}

#[test]
fn handshake_random_and_bitflipped_bytes_never_panic() {
    // Random bytes of varied length.
    let mut state = 0x00C0_FFEE_1234_5678u64;
    for _ in 0..20_000u32 {
        let len = (lcg(&mut state) as usize) % 96;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push(lcg(&mut state));
        }
        no_panic_handshake(&bytes);
    }
    // Bit-flips of a valid handshake at every position (present-bit and unknown-field mutations may
    // decode to a DIFFERENT-but-valid descriptor; we assert only that decode never panics — a handshake
    // is not byte-stable under mutation because unknown present bits are dropped on re-encode).
    let good = Capabilities::full("flip-me").to_handshake();
    for pos in 0..good.len() {
        for bit in 0..8u32 {
            let mut bad = good.clone();
            bad[pos] ^= 1 << bit;
            no_panic_handshake(&bad);
        }
    }
}

#[test]
fn handshake_wire_version_mismatch_is_a_typed_version_error() {
    // A backend advertising a version the guest does not speak must fail cleanly at negotiation with a
    // typed version error — NOT a later runtime BadTag after the app committed to a path. Both the
    // too-new and too-old directions, and a guest that is itself newer than the backend, are rejected.
    for backend_version in [WIRE_VERSION + 1, WIRE_VERSION - 1, WIRE_VERSION + 7] {
        let mut caps = Capabilities::full("mismatch");
        caps.wire_version = backend_version;
        // The handshake still DECODES structurally (the version check is a negotiation concern).
        let bytes = caps.to_handshake();
        let decoded = Capabilities::from_handshake(&bytes).expect("handshake decodes structurally");
        assert_eq!(decoded.wire_version, backend_version);
        // A guest pinned at WIRE_VERSION negotiating against it gets a typed Unsupported version error.
        let req = FeatureRequest { wire_version: WIRE_VERSION, ..Default::default() };
        assert_eq!(
            decoded.negotiate(&req),
            Err(GpuError::Unsupported("capability: wire version mismatch")),
            "backend v{backend_version} vs guest v{WIRE_VERSION} must be a typed version mismatch"
        );
    }
    // The matching version negotiates cleanly (the mismatch check is not over-eager).
    let caps = Capabilities::full("match");
    let req = FeatureRequest { wire_version: caps.wire_version, ..Default::default() };
    assert_eq!(caps.negotiate(&req), Ok(()));
}

// ---------------------------------------------------------------------------------------------------
// 3. end-to-end: hostile bytes → decode_stream → runtime::submit (decode → validate → account → dispatch)
// ---------------------------------------------------------------------------------------------------

/// A fresh per-connection session over the CPU reference oracle (the full advertised IR surface).
fn fresh_session() -> (Session, CpuExecutor) {
    let exec = CpuExecutor::new();
    let caps = exec.capabilities();
    let limits = Limits::from_capabilities(caps);
    let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    (session, exec)
}

/// Decode a byte stream and run it through the WHOLE runtime pipeline against the oracle. Any stage's
/// typed error propagates; nothing here may panic on decodable-but-hostile input.
fn decode_then_submit(bytes: &[u8]) -> Result<()> {
    let cmds = decode_stream(bytes)?;
    let (mut session, mut exec) = fresh_session();
    hl_gpu::runtime::submit(&mut session, &mut exec, bytes.len(), &cmds)?;
    Ok(())
}

#[test]
fn duplicate_id_in_a_decoded_stream_is_typed_duplicate_id() {
    // Two creates of the same fence id — a self-referential/duplicate stream. account treats a re-create
    // as a residency swap, so the DuplicateId surfaces from the executor's id table, end-to-end from bytes.
    let bytes = encode_stream(&[Cmd::CreateFence(1), Cmd::CreateFence(1)]);
    assert_eq!(decode_then_submit(&bytes), Err(GpuError::DuplicateId { kind: "fence", id: 1 }));
}

#[test]
fn dangling_id_in_a_decoded_stream_is_typed_unknown_id() {
    // Destroy a buffer that was never created (a dangling reference) — a typed UnknownId, not a panic.
    let bytes = encode_stream(&[Cmd::DestroyBuffer(7)]);
    assert_eq!(decode_then_submit(&bytes), Err(GpuError::UnknownId { kind: "buffer", id: 7 }));
}

#[test]
fn absurd_buffer_size_in_a_decoded_stream_is_resource_limit_not_oom() {
    // A CreateBuffer declaring u64::MAX bytes is rejected at VALIDATE against the negotiated per-buffer
    // ceiling — BEFORE the executor ever attempts `vec![0u8; size]` (which would OOM-abort the host).
    let bytes = encode_stream(&[Cmd::CreateBuffer(
        1,
        BufferDesc { size: u64::MAX, usage: 0, label: String::new() },
    )]);
    assert_eq!(decode_then_submit(&bytes), Err(GpuError::ResourceLimit("buffer bytes")));
}

#[test]
fn oversized_texture_footprint_in_a_decoded_stream_is_resource_limit() {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::protocol::model::enums::{TextureDim, TextureFormat};
    // A texture whose declared dimensions exceed the negotiated 2D ceiling is a typed ResourceLimit — the
    // footprint is never materialized. Dims over max_texture_2d (16384 for the oracle).
    let bytes = encode_stream(&[Cmd::CreateTexture(
        2,
        TextureDesc {
            width: 100_000,
            height: 100_000,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: 0,
            label: String::new(),
        },
    )]);
    assert_eq!(decode_then_submit(&bytes), Err(GpuError::ResourceLimit("texture dimensions")));
}

#[test]
fn the_whole_pipeline_never_panics_on_hostile_bytes() {
    // Random bytes and bit-flip mutations of a valid stream driven through decode → validate → account →
    // dispatch: decode either rejects them or the runtime returns a typed error — the pipeline never
    // panics, hangs, or OOM-aborts. (The existing fuzz corpora stop at decode_stream; this covers the
    // runtime stages too.)
    let base = encode_stream(&[
        Cmd::CreateBuffer(1, BufferDesc { size: 256, usage: 0x3F, label: "b".into() }),
        Cmd::CreateFence(2),
        Cmd::DestroyFence(2),
        Cmd::DestroyBuffer(1),
    ]);
    // The base stream itself runs clean end-to-end.
    assert!(decode_then_submit(&base).is_ok(), "the well-formed base stream must submit cleanly");

    let mut state = 0xDEAD_BEEF_0BAD_F00Du64;
    for _ in 0..20_000u32 {
        // Half random bytes, half mutated-valid stream.
        let bytes = if lcg(&mut state) & 1 == 0 {
            let len = (lcg(&mut state) as usize) % 200;
            let mut b = Vec::with_capacity(len);
            for _ in 0..len {
                b.push(lcg(&mut state));
            }
            b
        } else {
            let mut b = base.clone();
            let muts = 1 + (lcg(&mut state) % 4) as usize;
            for _ in 0..muts {
                if b.is_empty() {
                    break;
                }
                let pos = (lcg(&mut state) as usize) % b.len();
                b[pos] = lcg(&mut state);
            }
            b
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = decode_then_submit(&bytes);
        }));
        assert!(r.is_ok(), "decode→submit PANICKED on {} bytes: {:02x?}", bytes.len(), bytes);
    }
}
