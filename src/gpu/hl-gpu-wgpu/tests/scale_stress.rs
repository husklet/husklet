//! SCALE + PERF-STRESS battery for the `WgpuExecutor` (task #245).
//!
//! The small `perf_microbench` proves the neutral render path is non-degenerate on the CPU oracle. THIS
//! suite drives the SAME executor the conformance battery uses — the real wgpu/naga/lavapipe backend — but
//! at LARGE workloads, and every test asserts BOTH correctness (exact readback) AND a scaling / leak /
//! throughput property. The thresholds are deliberately generous: their only job is to trip on a genuine
//! O(n²) cliff, a per-frame leak, or a residency that never returns to baseline — never to flake on a slow
//! shared box. Every measured figure is PRINTED so a human reading the log sees the real numbers.
//!
//! Structure is DETERMINISTIC: every loop count is a fixed constant (never wall-clock-bounded), so the work
//! is identical run-to-run and only the elapsed time varies. All ceilings are ENV-overridable for the rare
//! pathologically-slow box (see [`env_f64`]).
//!
//! Each test acquires its own executor and SKIPS (returns) if no adapter is reachable, mirroring the rest of
//! the wgpu suite so a host with no Vulkan ICD still passes.

mod common;
use common::*;

use std::time::Instant;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::BufferId;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// Packed vertex-attribute format wire word (`comps | (kind<<8) | (norm<<16)`): a plain f32 vector is
// `comps`, kind=0 (float), norm=0 — so vec2 f32 → 2, vec4 f32 → 4.
const VFMT_F32X2: u32 = 2;
const VFMT_F32X4: u32 = 4;

/// Read an `f64` threshold from `var`, falling back to `default` (a slow CI box can relax a tripwire without
/// touching the source).
fn env_f64(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Median of a slice (copies + sorts; used only on small timing vectors).
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// Acquire the wgpu executor, or `None` if no adapter is reachable (the whole test then skips).
fn try_exec() -> Option<WgpuExecutor> {
    WgpuExecutor::new(DeviceConfig::default()).ok()
}

// ===================================================================================================
// Test 1 — MANY DRAWS, ONE FRAME: thousands of individual draws into one target, exact per-cell readback.
// ===================================================================================================
//
// A tiled grid of NUM_DRAWS cells; draw `i` renders a quad EXACTLY covering cell `i` with a per-draw color
// fed from a single shared vertex buffer (`first_vertex = i*4`). Because the cells tile the target with
// pixel-aligned edges, EVERY output pixel belongs to exactly one draw, so the readback is checked in full:
// a dropped draw leaves its cell at the clear color (fail), a mis-placed draw corrupts two cells (fail).
// This stresses the render-pass encoder's per-draw replay path (submit.rs) for an O(n²) cliff.

#[path = "scale_stress/draw.rs"]
mod draw;
#[path = "scale_stress/lifetime.rs"]
mod lifetime;
#[path = "scale_stress/multipass.rs"]
mod multipass;
#[path = "scale_stress/pipeline.rs"]
mod pipeline;
#[path = "scale_stress/resource.rs"]
mod resource;
#[path = "scale_stress/steady.rs"]
mod steady;
