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
    Capabilities, Cmd, CpuExecutor, FakeClock, FeatureRequest, GlobalLedger, GpuError, GpuExecutor,
    Limits, Result, Session, WIRE_VERSION,
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
        (
            "render_pipeline_vertex_buffers",
            render_pipeline_vertex_buffers_bomb(),
        ),
        (
            "render_pipeline_color_targets",
            render_pipeline_color_targets_bomb(),
        ),
        ("vertex_layout_attrs", vertex_layout_attrs_bomb()),
        ("buffer_label_str", buffer_label_str_bomb()),
    ]
}

/// Reproducible LCG byte source (index-derived, no rng/clock).
fn lcg(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 56) as u8
}

// ---------------------------------------------------------------------------------------------------
// 1. decode-bombs are typed errors AND provably do not preallocate.
// ---------------------------------------------------------------------------------------------------

#[path = "wire_fuzz/allocation.rs"]
mod allocation;
#[path = "wire_fuzz/handshake.rs"]
mod handshake;
#[path = "wire_fuzz/runtime.rs"]
mod runtime;
