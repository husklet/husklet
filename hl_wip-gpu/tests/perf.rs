//! Performance BASELINES for the hl_wip-gpu hot paths, using only `std::time` (no criterion / external
//! bench crate — the build host is offline). Every `#[test]` here WARMS UP, times a batch, and PRINTS a
//! labeled number (run with `--nocapture` to see them):
//!
//! ```text
//! cargo test --release --offline --manifest-path hl_wip-gpu/Cargo.toml --test perf -- --nocapture
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

use hl_gpu::protocol::codec::{decode_stream, encode_stream};
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
            Param { width: 8, offset: 0, is_ptr: true, region: 0 },
            Param { width: 8, offset: 8, is_ptr: true, region: 1 },
            Param { width: 8, offset: 16, is_ptr: true, region: 2 },
            Param { width: 4, offset: 24, is_ptr: false, region: 0 },
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
            Inst::MovSReg { d: 4, sreg: SR_NTID_X },
            Inst::MovSReg { d: 5, sreg: SR_CTAID_X },
            Inst::MovSReg { d: 6, sreg: SR_TID_X },
            Inst::IMad { d: 7, a: Op::Reg(5), b: Op::Reg(4), c: Op::Reg(6) },
            Inst::Setp { d: 8, a: Op::Reg(7), b: Op::Reg(3), cmp: CMP_GE, unsigned: false },
            Inst::Bra { target: 21, pred: Some((8, false)) },
            Inst::Cvta { d: 9, s: 0 },
            Inst::IMul { d: 10, a: Op::Reg(7), b: Op::ImmI(4), wide: true, unsigned: false },
            Inst::IAdd { d: 11, a: Op::Reg(9), b: Op::Reg(10), wide: true },
            Inst::Cvta { d: 12, s: 1 },
            Inst::IAdd { d: 13, a: Op::Reg(12), b: Op::Reg(10), wide: true },
            Inst::LdGlobal { d: 14, addr: 13, off: 0, ty: gty::F32 },
            Inst::LdGlobal { d: 15, addr: 11, off: 0, ty: gty::F32 },
            Inst::FAdd { d: 16, a: Op::Reg(15), b: Op::Reg(14) },
            Inst::Cvta { d: 17, s: 2 },
            Inst::IAdd { d: 18, a: Op::Reg(17), b: Op::Reg(10), wide: true },
            Inst::StGlobal { addr: 18, off: 0, src: Op::Reg(16), ty: gty::F32 },
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
            BufferDesc { size: 4096, usage: buffer_usage::STORAGE | buffer_usage::COPY_DST, label: String::new() },
        ));
        cmds.push(Cmd::WriteBuffer { id, offset: 0, data: chunk.clone() });
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

#[test]
fn perf_codec_encode_decode_throughput() {
    let cmds = representative_stream();
    let bytes = encode_stream(&cmds).len();
    let mb = bytes as f64 / (1024.0 * 1024.0);

    // Warm up (fill caches / branch predictors).
    for _ in 0..3 {
        let e = encode_stream(&cmds);
        let _ = decode_stream(&e).unwrap();
    }

    let iters = 50u32;

    let t0 = Instant::now();
    let mut last = Vec::new();
    for _ in 0..iters {
        last = encode_stream(&cmds);
    }
    let enc_elapsed = t0.elapsed();
    let enc_mbps = (mb * iters as f64) / enc_elapsed.as_secs_f64();

    let t1 = Instant::now();
    for _ in 0..iters {
        let _ = decode_stream(&last).unwrap();
    }
    let dec_elapsed = t1.elapsed();
    let dec_mbps = (mb * iters as f64) / dec_elapsed.as_secs_f64();

    println!("perf: codec encode = {enc_mbps:.1} MB/s ({iters} iters, {bytes} bytes/stream)");
    println!("perf: codec decode = {dec_mbps:.1} MB/s ({iters} iters, {bytes} bytes/stream)");

    // Loose floors: a hang or a catastrophic regression fails; normal variance passes.
    assert!(bytes > 512 * 1024, "stream should be a few MB, got {bytes} bytes");
    assert!(enc_mbps > 1.0, "encode throughput collapsed: {enc_mbps} MB/s");
    assert!(dec_mbps > 1.0, "decode throughput collapsed: {dec_mbps} MB/s");
}

// -------------------------------------------------------------------------------------------------
// 2. CPU compute throughput — a real vecadd over 1M f32 via InProcessCommandSink<CpuExecutor>
// -------------------------------------------------------------------------------------------------

#[test]
fn perf_cpu_compute_vecadd_throughput() {
    let n: u32 = 1 << 20; // 1,048,576 elements
    let buf_bytes = (n as u64) * 4;

    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, vecadd_program());
    let mut sink = InProcessCommandSink::new(exec);

    // n at param offset 24; the three u64 pointer slots are ignored by the interpreter.
    let mut param = vec![0u8; 28];
    param[24..28].copy_from_slice(&n.to_le_bytes());
    let a = vec![0u8; buf_bytes as usize]; // 0.0f
    let b = vec![0u8; buf_bytes as usize];

    // One-time setup submit: shader + pipeline + buffers + input uploads + bind group.
    sink.submit(&[
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: kernel_words() },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "vecadd".into() }, label: String::new() },
        ),
        Cmd::CreateBuffer(1, BufferDesc { size: 28, usage: buffer_usage::STORAGE, label: String::new() }),
        Cmd::CreateBuffer(2, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
        Cmd::CreateBuffer(3, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
        Cmd::CreateBuffer(4, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC, label: String::new() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: param },
        Cmd::WriteBuffer { id: 2, offset: 0, data: a },
        Cmd::WriteBuffer { id: 3, offset: 0, data: b },
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 28 } },
                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: buf_bytes } },
                    BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: buf_bytes } },
                    BindEntry { binding: 3, resource: BindResource::Buffer { id: 4, offset: 0, size: buf_bytes } },
                ],
            },
        ),
    ])
    .expect("compute setup submit");

    // block.x = 4, so cover n elements with n/4 groups.
    let groups = n / 4;
    let dispatch = Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: groups, y: 1, z: 1 },
            Enc::EndComputePass,
        ],
        signal: None,
    });

    // Warm up one dispatch.
    sink.submit(std::slice::from_ref(&dispatch)).expect("warmup dispatch");

    let iters = 3u32;
    let t0 = Instant::now();
    for _ in 0..iters {
        sink.submit(std::slice::from_ref(&dispatch)).expect("timed dispatch");
    }
    let elapsed = t0.elapsed();

    let per_dispatch_ms = elapsed.as_secs_f64() * 1e3 / iters as f64;
    let elems_per_s = (n as f64 * iters as f64) / elapsed.as_secs_f64();

    println!(
        "perf: cpu vecadd = {:.2}M elem/s, {:.2} ms/dispatch ({} iters, {} elems/dispatch)",
        elems_per_s / 1e6,
        per_dispatch_ms,
        iters,
        n
    );

    // Sanity-check correctness once (0 + 0 == 0) and prove the readback path works.
    let out = sink.read_buffer(BufferId(4), 0, 16).expect("readback c");
    assert_eq!(&out, &[0u8; 16], "vecadd of zeros must be zero");

    assert!(elems_per_s > 10_000.0, "compute throughput collapsed: {elems_per_s} elem/s");
    assert!(per_dispatch_ms < 5_000.0, "single dispatch is implausibly slow: {per_dispatch_ms} ms");
}

// -------------------------------------------------------------------------------------------------
// transport harness — a runtime-backed host serving submit + readback (mirrors tests/readback.rs)
// -------------------------------------------------------------------------------------------------

struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-perf-{tag}-{}-{:?}.sock",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
}
impl RuntimeHost {
    fn new() -> Self {
        let exec = CpuExecutor::new();
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
        Self { session, exec }
    }
}
impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        let frame_bytes = encode_stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => Verdict::Ack,
            Err(_) => Verdict::Nack,
        }
    }
    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
}

// -------------------------------------------------------------------------------------------------
// 3. transport round-trip — submit+ack latency, and device->host readback latency + MB/s
// -------------------------------------------------------------------------------------------------

#[test]
fn perf_transport_submit_and_readback_latency() {
    let sock = TempSock::new("rt");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());

    // A readback buffer we can hammer: create + fill a 1 MiB buffer once.
    let rb_bytes = 1usize << 20;
    sink.submit(&[
        Cmd::CreateBuffer(
            1,
            BufferDesc { size: rb_bytes as u64, usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC, label: String::new() },
        ),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0x5Au8; rb_bytes] },
    ])
    .expect("readback buffer upload");

    // --- submit+ack latency: a tiny batch, over the already-open connection ---
    // Unique fence ids per submit so the persistent host session never rejects a duplicate create.
    for i in 0..20u32 {
        sink.submit(&[Cmd::CreateFence(1000 + i)]).expect("warmup submit");
    }
    let k = 300u32;
    let mut samples = Vec::with_capacity(k as usize);
    for i in 0..k {
        let batch = [Cmd::CreateFence(2000 + i)];
        let t = Instant::now();
        sink.submit(&batch).expect("timed submit");
        samples.push(t.elapsed());
    }
    let submit_mean = mean(&samples);
    let submit_med = median(samples);
    println!(
        "perf: transport submit+ack = {:.1} us mean, {:.1} us median ({} iters, small batch)",
        us(submit_mean),
        us(submit_med),
        k
    );

    // --- readback latency + throughput: read the 1 MiB buffer repeatedly ---
    for _ in 0..5 {
        let _ = sink.read_buffer(BufferId(1), 0, rb_bytes).expect("warmup readback");
    }
    let kr = 50u32;
    let mut rb_samples = Vec::with_capacity(kr as usize);
    for _ in 0..kr {
        let t = Instant::now();
        let got = sink.read_buffer(BufferId(1), 0, rb_bytes).expect("timed readback");
        rb_samples.push(t.elapsed());
        assert_eq!(got.len(), rb_bytes);
    }
    let rb_mean = mean(&rb_samples);
    let rb_med = median(rb_samples);
    let rb_mbps = (rb_bytes as f64 / (1024.0 * 1024.0)) / rb_mean.as_secs_f64();
    println!(
        "perf: transport readback = {:.1} us mean, {:.1} us median, {:.1} MB/s ({} iters, {} bytes)",
        us(rb_mean),
        us(rb_med),
        rb_mbps,
        kr,
        rb_bytes
    );

    drop(sink);
    server.join().unwrap();

    // Generous ceilings / floors: catch a hang or a total collapse, ignore variance.
    assert!(us(submit_med) < 500_000.0, "submit latency implausibly high: {} us", us(submit_med));
    assert!(us(rb_med) < 2_000_000.0, "readback latency implausibly high: {} us", us(rb_med));
    assert!(rb_mbps > 0.1, "readback throughput collapsed: {rb_mbps} MB/s");
}

// -------------------------------------------------------------------------------------------------
// 4. in-process submit latency — the socket-free path, for comparison against (3)
// -------------------------------------------------------------------------------------------------

#[test]
fn perf_inprocess_submit_latency() {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    // Distinct fence ids so validation never rejects a duplicate create.
    for i in 0..20u32 {
        sink.submit(&[Cmd::CreateFence(i + 1)]).expect("warmup submit");
    }
    let k = 500u32;
    let mut samples = Vec::with_capacity(k as usize);
    for i in 0..k {
        let batch = [Cmd::CreateFence(100 + i)];
        let t = Instant::now();
        sink.submit(&batch).expect("timed submit");
        samples.push(t.elapsed());
    }
    let m = mean(&samples);
    let med = median(samples);
    println!(
        "perf: in-process submit = {:.2} us mean, {:.2} us median ({} iters, small batch)",
        us(m),
        us(med),
        k
    );
    assert!(us(med) < 100_000.0, "in-process submit latency implausibly high: {} us", us(med));
}
