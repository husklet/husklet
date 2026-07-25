use super::*;

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
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "vecadd".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 28,
                usage: buffer_usage::STORAGE,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            2,
            BufferDesc {
                size: buf_bytes,
                usage: buffer_usage::STORAGE,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            3,
            BufferDesc {
                size: buf_bytes,
                usage: buffer_usage::STORAGE,
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            4,
            BufferDesc {
                size: buf_bytes,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: param,
        },
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: a,
        },
        Cmd::WriteBuffer {
            id: 3,
            offset: 0,
            data: b,
        },
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 28,
                        },
                    },
                    BindEntry {
                        binding: 1,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: buf_bytes,
                        },
                    },
                    BindEntry {
                        binding: 2,
                        resource: BindResource::Buffer {
                            id: 3,
                            offset: 0,
                            size: buf_bytes,
                        },
                    },
                    BindEntry {
                        binding: 3,
                        resource: BindResource::Buffer {
                            id: 4,
                            offset: 0,
                            size: buf_bytes,
                        },
                    },
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
            Enc::Dispatch {
                x: groups,
                y: 1,
                z: 1,
            },
            Enc::EndComputePass,
        ],
        signal: None,
    });

    // Warm up one dispatch.
    sink.submit(std::slice::from_ref(&dispatch))
        .expect("warmup dispatch");

    let iters = 3u32;
    let t0 = Instant::now();
    for _ in 0..iters {
        sink.submit(std::slice::from_ref(&dispatch))
            .expect("timed dispatch");
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

    assert!(
        elems_per_s > 10_000.0,
        "compute throughput collapsed: {elems_per_s} elem/s"
    );
    assert!(
        per_dispatch_ms < 5_000.0,
        "single dispatch is implausibly slow: {per_dispatch_ms} ms"
    );
}

// -------------------------------------------------------------------------------------------------
// transport harness — a runtime-backed host serving submit + readback (mirrors tests/readback.rs)
// -------------------------------------------------------------------------------------------------
