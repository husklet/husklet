//! Performance BASELINES for the hl-gpu hot paths, using only `std::time` (no criterion / external
//! bench crate — the build host is offline). Every `#[test]` here WARMS UP, times a batch, and PRINTS a
//! labeled number (run with `--nocapture` to see them):
//!
//! ```text
//! cargo test --release --offline --manifest-path hl-gpu/Cargo.toml --test perf -- --nocapture
//! ```
//!
//! Each test asserts only a LOOSE sanity bound (a tiny throughput floor / a generous latency ceiling), so a
//! real regression or a hang FAILS while ordinary run-to-run variance PASSES. These are baselines, not gates.
//!
//! Numbers were captured on aarch64 (arm64) Linux. Prefer `--release` for realistic figures; a debug build
//! runs the same asserts but is roughly an order of magnitude slower (the loose bounds still hold).
//!
//! The four covered hot paths:
//!   1. `codec` encode/decode throughput (MB/s) over a representative multi-megabyte command stream.
//!   2. CPU compute throughput (elements/s, ms/dispatch) — a real `vecadd` kernel over a 1M-f32 buffer via
//!      `InProcessCommandSink<CpuExecutor>`.
//!   3. Transport round-trip latency (µs) over a real `UnixListener` + runtime-backed server: submit+ack, and
//!      device→host `read_buffer` (µs/readback + MB/s).
//!   4. In-process submit latency (µs) for the socket-free path, as a comparison point against (3).

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Capabilities, Cmd, CommandBuffer, CommandSink, ConnectionHandler, CpuExecutor, Enc,
    FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink, Limits, ReadbackRequest,
    RemoteCommandSink, Session, ShaderPayloadKind,
};

// -------------------------------------------------------------------------------------------------
// small stats helpers
// -------------------------------------------------------------------------------------------------

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

fn mean(xs: &[Duration]) -> Duration {
    let total: Duration = xs.iter().sum();
    total / (xs.len() as u32)
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

// -------------------------------------------------------------------------------------------------
// kernel IR (a real vecadd, identical to tests/conformance.rs — the PTX front-end is a driver concern)
// -------------------------------------------------------------------------------------------------

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

/// `c[i] = a[i] + b[i]` with `i = blockIdx*blockDim + tid` and an `if (i >= n) return;` guard.
fn vecadd_program() -> KernelProgram {
    KernelProgram {
        entry: "vecadd".into(),
        block: [4, 1, 1],
        params: vec![
            Param {
                width: 8,
                offset: 0,
                is_ptr: true,
                region: 0,
            },
            Param {
                width: 8,
                offset: 8,
                is_ptr: true,
                region: 1,
            },
            Param {
                width: 8,
                offset: 16,
                is_ptr: true,
                region: 2,
            },
            Param {
                width: 4,
                offset: 24,
                is_ptr: false,
                region: 0,
            },
        ],
        param_bytes: 28,
        num_regions: 3,
        shared_bytes: 0,
        reg_count: 19,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::LdParam { d: 2, param: 2 },
            Inst::LdParam { d: 3, param: 3 },
            Inst::MovSReg {
                d: 4,
                sreg: SR_NTID_X,
            },
            Inst::MovSReg {
                d: 5,
                sreg: SR_CTAID_X,
            },
            Inst::MovSReg {
                d: 6,
                sreg: SR_TID_X,
            },
            Inst::IMad {
                d: 7,
                a: Op::Reg(5),
                b: Op::Reg(4),
                c: Op::Reg(6),
            },
            Inst::Setp {
                d: 8,
                a: Op::Reg(7),
                b: Op::Reg(3),
                cmp: CMP_GE,
                unsigned: false,
            },
            Inst::Bra {
                target: 21,
                pred: Some((8, false)),
            },
            Inst::Cvta { d: 9, s: 0 },
            Inst::IMul {
                d: 10,
                a: Op::Reg(7),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            },
            Inst::IAdd {
                d: 11,
                a: Op::Reg(9),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::Cvta { d: 12, s: 1 },
            Inst::IAdd {
                d: 13,
                a: Op::Reg(12),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::LdGlobal {
                d: 14,
                addr: 13,
                off: 0,
                ty: gty::F32,
            },
            Inst::LdGlobal {
                d: 15,
                addr: 11,
                off: 0,
                ty: gty::F32,
            },
            Inst::FAdd {
                d: 16,
                a: Op::Reg(15),
                b: Op::Reg(14),
            },
            Inst::Cvta { d: 17, s: 2 },
            Inst::IAdd {
                d: 18,
                a: Op::Reg(17),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::StGlobal {
                addr: 18,
                off: 0,
                src: Op::Reg(16),
                ty: gty::F32,
            },
            Inst::Ret,
        ],
    }
}

// -------------------------------------------------------------------------------------------------
// 1. codec throughput — encode/decode a representative multi-MB command stream
// -------------------------------------------------------------------------------------------------

/// A representative "residency upload + dispatch" stream: many CreateBuffer + WriteBuffer(4 KiB) ops with
/// interleaved compute Submits — the shape a driver streams to the host each frame.
fn representative_stream() -> Vec<Cmd> {
    let mut cmds = Vec::new();
    let chunk = vec![0xABu8; 4096]; // 4 KiB payload per write
    let n = 256usize;
    for i in 0..n {
        let id = (i as u32) + 1;
        cmds.push(Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 4096,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id,
            offset: 0,
            data: chunk.clone(),
        });
        if i % 8 == 0 {
            cmds.push(Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::Dispatch { x: 64, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }));
        }
    }
    cmds
}

#[path = "perf/codec.rs"]
mod codec;
#[path = "perf/compute.rs"]
mod compute;
#[path = "perf/draw.rs"]
mod draw;
#[path = "perf/inprocess.rs"]
mod inprocess;
#[path = "perf/transport.rs"]
mod transport;
