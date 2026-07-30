//! STAGE-BY-STAGE profile of one glmark2-shaped frame through the whole IR path, on the real wgpu
//! executor (task: per-draw cost hunt, 2026-07-30).
//!
//! `glmark2 -b ideas` measured 358 ms/frame at ~1782 encoder ops per submit — ~200 µs per command, which
//! no GPU-bound scene looks like. This benchmark reproduces that SHAPE and times each stage separately so
//! the cost is attributed rather than guessed: encode → decode → validate → account → executor dispatch,
//! with the executor's own [`hl_gpu_wgpu::Profile`] splitting dispatch into buffer creates, bind-group
//! creates, and the render pass itself.
//!
//! Two shapes are measured, differing ONLY in resource reuse:
//!   * `per_draw` — a fresh uniform buffer + a fresh bind group per draw. This is exactly what
//!     `hl-gl`'s `service/frame/lower.rs` emits today (`alloc_buffer_ir` + `alloc_bind_group_ir` inside
//!     the per-draw lowering).
//!   * `shared` — one uniform buffer + one bind group for the whole frame, bound once. The floor the
//!     same draw count can reach.
//!
//! The delta is the per-draw resource-churn cost, in microseconds per draw.
//!
//! Both shapes read the target back and assert their own exact pixels, so a "faster" shape that stopped
//! drawing fails.
//!
//! Run:
//! ```text
//! cargo test --release --offline -p hl-gpu-wgpu --test frame_profile -- --nocapture
//! ```
//! Thresholds are loose ceilings (see [`env_us`]): they trip on an order-of-magnitude regression, never on
//! ordinary variance on a shared box. Every figure is printed.

#[path = "frame_profile/batch.rs"]
mod batch;
mod gpu_harness;

use batch::{expected, frame_batch, setup_batch, Shape, CELL, DRAWS, GRID, H, TARGET, W};
use gpu_harness::{new_session, px};

/// Timed frames per shape.
const FRAMES: usize = 5;

/// Read an override for a loose ceiling, so a pathologically slow box can relax a tripwire.
fn env_us(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn try_exec() -> WgpuExecutor {
    WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to profile the wgpu executor")
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

use std::time::{Duration, Instant};

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{Cmd, CommandBuffer, Enc};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// One stage's total across the timed frames.
#[derive(Default)]
struct Stages {
    encode: Duration,
    decode: Duration,
    validate: Duration,
    account: Duration,
    dispatch: Duration,
}

impl Stages {
    fn print(&self, label: &str, frames: usize, commands: usize) {
        let n = frames as f64;
        let per_cmd = |d: Duration| us(d) / n / commands as f64;
        println!(
            "perf[{label}]: frame = {:.0} us  ({commands} cmds, {DRAWS} draws)",
            (us(self.encode)
                + us(self.decode)
                + us(self.validate)
                + us(self.account)
                + us(self.dispatch))
                / n
        );
        println!(
            "perf[{label}]:   encode   {:8.1} us/frame  {:6.3} us/cmd",
            us(self.encode) / n,
            per_cmd(self.encode)
        );
        println!(
            "perf[{label}]:   decode   {:8.1} us/frame  {:6.3} us/cmd",
            us(self.decode) / n,
            per_cmd(self.decode)
        );
        println!(
            "perf[{label}]:   validate {:8.1} us/frame  {:6.3} us/cmd",
            us(self.validate) / n,
            per_cmd(self.validate)
        );
        println!(
            "perf[{label}]:   account  {:8.1} us/frame  {:6.3} us/cmd",
            us(self.account) / n,
            per_cmd(self.account)
        );
        println!(
            "perf[{label}]:   dispatch {:8.1} us/frame  {:6.3} us/cmd",
            us(self.dispatch) / n,
            per_cmd(self.dispatch)
        );
    }

    fn total_us_per_frame(&self, frames: usize) -> f64 {
        (us(self.encode)
            + us(self.decode)
            + us(self.validate)
            + us(self.account)
            + us(self.dispatch))
            / frames as f64
    }
}

/// Run `FRAMES` frames of `shape`, timing every stage, asserting the readback, and returning the totals.
fn profile(shape: Shape) -> (Stages, f64) {
    let mut exec = try_exec();
    println!("perf: adapter = {}", exec.adapter_name());
    let mut s = new_session(&exec);
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &setup_batch()).expect("setup batch");

    let batch = frame_batch(shape);
    let commands = batch.len();
    let mut stages = Stages::default();

    // Warm up: first frame compiles nothing new but does prime wgpu's allocators and the pass path.
    run_frame(&mut s, &mut exec, &batch, &mut Stages::default());
    check_pixels(&mut s, &mut exec, shape);

    exec.enable_profile();
    for _ in 0..FRAMES {
        run_frame(&mut s, &mut exec, &batch, &mut stages);
    }
    let profile = exec.profile().expect("profile enabled");
    let n = FRAMES as f64;
    println!(
        "perf: executor bind_group_creates={} ({:.1} us/frame)  buffer_creates={} ({:.1} us/frame)",
        profile.bind_groups.count,
        us(profile.bind_groups.elapsed) / n,
        profile.buffers.count,
        us(profile.buffers.elapsed) / n,
    );
    println!(
        "perf: executor buffer_writes={} ({:.1} us/frame)  destroys={} ({:.1} us/frame)",
        profile.buffer_writes.count,
        us(profile.buffer_writes.elapsed) / n,
        profile.destroys.count,
        us(profile.destroys.elapsed) / n,
    );
    println!(
        "perf: executor draw_bind_groups={} ({:.1} us/frame, {:.3} us each)  render_passes={} native_submits={}  waits={} ({:.1} us/frame)",
        profile.draw_bind_groups.count,
        us(profile.draw_bind_groups.elapsed) / n,
        if profile.draw_bind_groups.count == 0 {
            0.0
        } else {
            us(profile.draw_bind_groups.elapsed) / profile.draw_bind_groups.count as f64
        },
        profile.render_passes.count,
        profile.native_submissions,
        profile.waits.count,
        us(profile.waits.elapsed) / n,
    );
    check_pixels(&mut s, &mut exec, shape);
    let total = stages.total_us_per_frame(FRAMES);
    stages.print(
        if shape == Shape::PerDraw {
            "per_draw"
        } else {
            "shared"
        },
        FRAMES,
        commands,
    );
    (stages, total)
}

/// Encode → decode → validate → account → dispatch one frame, accumulating each stage.
fn run_frame(s: &mut hl_gpu::Session, exec: &mut WgpuExecutor, batch: &[Cmd], stages: &mut Stages) {
    let t = Instant::now();
    let wire = hl_gpu::Encoder::stream(batch);
    stages.encode += t.elapsed();

    let t = Instant::now();
    let decoded = hl_gpu::Decoder::stream(&wire).expect("decode");
    stages.decode += t.elapsed();

    let t = Instant::now();
    hl_gpu::runtime::service::validate::validate(&s.limits, wire.len(), &decoded)
        .expect("validate");
    stages.validate += t.elapsed();

    let t = Instant::now();
    s.charge_frame(&decoded).expect("account");
    stages.account += t.elapsed();

    let t = Instant::now();
    hl_gpu::runtime::service::dispatch::dispatch(s, exec, &decoded).expect("dispatch");
    stages.dispatch += t.elapsed();
}

/// Read the target back and assert every cell carries its draw's tint — the shape drew what it claimed.
fn check_pixels(s: &mut hl_gpu::Session, exec: &mut WgpuExecutor, shape: Shape) {
    let bytes = (W * H * 4) as usize;
    hl_gpu::runtime::submit(
        s,
        exec,
        0,
        &[
            Cmd::CreateBuffer(
                9000,
                BufferDesc {
                    size: bytes as u64,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToBuffer {
                    src: TARGET,
                    mip: 0,
                    dst: 9000,
                    dst_offset: 0,
                    bytes_per_row: W * 4,
                    width: W,
                    height: H,
                }],
                signal: None,
            }),
        ],
    )
    .expect("readback submit");
    let plane = hl_gpu::runtime::service::dispatch::read_buffer(
        s,
        &*exec,
        hl_gpu::BufferId(9000),
        0,
        bytes,
    )
    .expect("read_buffer");
    hl_gpu::runtime::submit(s, exec, 0, &[Cmd::DestroyBuffer(9000)]).expect("readback teardown");
    for i in 0..DRAWS {
        let gx = (i as u32) % GRID;
        let gy = (i as u32) / GRID;
        let got = px(&plane, W, gx * CELL + 1, gy * CELL + 1);
        assert_eq!(
            got,
            expected(shape, i),
            "cell {i} ({gx},{gy}) wrong: the shape did not draw what it claimed"
        );
    }
}

#[test]
fn frame_profile_per_draw_resources() {
    let (_, total) = profile(Shape::PerDraw);
    let ceiling = env_us("HL_PERF_FRAME_PER_DRAW_US", 2_000_000.0);
    assert!(
        total < ceiling,
        "per-draw frame collapsed: {total:.0} us/frame > {ceiling:.0}"
    );
}

#[test]
fn frame_profile_shared_resources() {
    let (_, total) = profile(Shape::Shared);
    let ceiling = env_us("HL_PERF_FRAME_SHARED_US", 2_000_000.0);
    assert!(
        total < ceiling,
        "shared-resource frame collapsed: {total:.0} us/frame > {ceiling:.0}"
    );
}
