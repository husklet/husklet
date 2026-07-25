use super::*;

/// Concurrent COMPUTE sessions (each running a real vecadd over known inputs, many times) racing against
/// concurrent CHURN sessions (creating/destroying resources) on the same shared account. Every compute
/// session must read back the exact `a[i]+b[i]` result on every round — a session-crossing corruption or
/// a shared-account race would perturb a result — and after everything drops the account is baseline.
#[test]
fn mixed_load() {
    with_timeout(180, || {
        const COMPUTE_THREADS: u32 = 8;
        const CHURN_THREADS: u32 = 8;
        const ROUNDS: usize = 12;
        const N: u32 = 256; // vecadd elements per dispatch
        const CHURN_BUFS: u32 = 32;
        const CHURN_SIZE: u64 = 4096;

        let global = GlobalLedger::unbounded();
        // Count completed compute rounds across all threads, to assert the load actually ran.
        let completed = Arc::new(AtomicU64::new(0));

        thread::scope(|scope| {
            // --- compute workers: real vecadd, unique per-thread inputs, verified readback every round ---
            for t in 0..COMPUTE_THREADS {
                let global = &global;
                let completed = Arc::clone(&completed);
                scope.spawn(move || {
                    let buf_bytes = (N as u64) * 4;
                    let mut exec = CpuExecutor::new();
                    exec.define_kernel(1, vecadd_program());
                    let limits = Limits::from_capabilities(exec.capabilities());
                    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
                    let mut s = InProcessCommandSink::with_session(session, exec);

                    // Per-thread inputs: a[i] = i + t*1000, b[i] = 2*i + t, so every thread's result differs
                    // and a cross-session bleed would produce the wrong sum.
                    let a: Vec<u8> = (0..N)
                        .flat_map(|i| ((i as f32) + (t as f32) * 1000.0).to_le_bytes())
                        .collect();
                    let b: Vec<u8> = (0..N)
                        .flat_map(|i| ((2 * i) as f32 + t as f32).to_le_bytes())
                        .collect();
                    let mut param = vec![0u8; 28];
                    param[24..28].copy_from_slice(&N.to_le_bytes());

                    // Setup: shader + pipeline + 4 buffers + input uploads + bind group.
                    s.submit(&[
                        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: vec![KERNEL_MAGIC, 0] },
                        Cmd::CreateComputePipeline(
                            1,
                            ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "vecadd".into() }, label: String::new() },
                        ),
                        Cmd::CreateBuffer(1, BufferDesc { size: 28, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(2, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(3, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(4, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC, label: String::new() }),
                        Cmd::WriteBuffer { id: 1, offset: 0, data: param },
                        Cmd::WriteBuffer { id: 2, offset: 0, data: a.clone() },
                        Cmd::WriteBuffer { id: 3, offset: 0, data: b.clone() },
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
                    .expect("compute setup");

                    let groups = N / 4;
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

                    for round in 0..ROUNDS {
                        s.submit(std::slice::from_ref(&dispatch)).expect("compute dispatch");
                        // Read the whole result buffer and verify c[i] == a[i] + b[i] for every element.
                        let out = s.read_buffer(BufferId(4), 0, buf_bytes as usize).expect("compute readback");
                        for i in 0..N as usize {
                            let bytes: [u8; 4] = out[i * 4..i * 4 + 4].try_into().unwrap();
                            let got = f32::from_le_bytes(bytes);
                            let expect = ((i as f32) + (t as f32) * 1000.0) + ((2 * i) as f32 + t as f32);
                            assert_eq!(
                                got, expect,
                                "compute thread {t} round {round} elem {i}: wrong result under mixed load (corruption/bleed)",
                            );
                        }
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                    // `s` drops → refunds this compute session's contribution.
                });
            }

            // --- churn workers: create/destroy pressure on the same shared account, concurrently ---
            for _ in 0..CHURN_THREADS {
                let global = &global;
                scope.spawn(move || {
                    let mut s = sink_on(global);
                    for _ in 0..ROUNDS * 4 {
                        let create: Vec<Cmd> =
                            (1..=CHURN_BUFS).map(|id| buffer(id, CHURN_SIZE)).collect();
                        s.submit(&create).expect("churn create");
                        // Write + read one to keep the executor genuinely busy.
                        s.submit(&[write(1, 0xE7, CHURN_SIZE as usize)])
                            .expect("churn write");
                        assert_eq!(s.read_buffer(BufferId(1), 0, 8).unwrap(), vec![0xE7; 8]);
                        let destroy: Vec<Cmd> = (1..=CHURN_BUFS).map(Cmd::DestroyBuffer).collect();
                        s.submit(&destroy).expect("churn destroy");
                        assert_eq!(s.session().residency_bytes(), 0);
                    }
                });
            }
        });

        // The compute load actually ran to completion on every thread and every round.
        assert_eq!(
            completed.load(Ordering::Relaxed),
            (COMPUTE_THREADS as u64) * (ROUNDS as u64),
            "not every compute round completed",
        );
        // All sessions dropped → shared account exactly at baseline (no corruption stranded residency).
        assert_eq!(
            global.residency_bytes(),
            0,
            "mixed load leaked shared residency"
        );
        assert_eq!(
            global.object_count(),
            0,
            "mixed load leaked shared object count"
        );
    });
}
