use super::*;

const T6_CYCLES: usize = 30;
const T6_BUFS: usize = 64;
const T6_TEXS: usize = 64;
const T6_BUF_BYTES: u64 = 256 << 10; // 256 KiB each

#[test]
fn no_resource_leak() {
    let mut exec = try_exec();
    let mut s = new_session(&exec);

    let baseline_live = s.resources.live_count();
    let baseline_bytes = s.account.ledger().residency_bytes();
    let baseline_global = s.global.residency_bytes();
    let baseline_objs = s.global.object_count();

    let mut peak_live = baseline_live;
    for cycle in 0..T6_CYCLES {
        // --- create many ---
        let mut create: Vec<Cmd> = Vec::with_capacity(T6_BUFS * 2 + T6_TEXS);
        for b in 0..T6_BUFS {
            let id = (b as u32) + 1;
            create.push(Cmd::CreateBuffer(
                id,
                BufferDesc {
                    size: T6_BUF_BYTES,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ));
        }
        for t in 0..T6_TEXS {
            let id = (t as u32) + 1;
            create.push(Cmd::CreateTexture(
                id,
                tex2d(
                    64,
                    64,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ));
        }
        // Write a cycle-distinct pattern into buffer 1 (COPY_DST) so a readback proves the fresh resource works.
        let marker = vec![(cycle as u8).wrapping_add(0x11); 32];
        create.push(Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: marker.clone(),
        });
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &create)
            .expect("create-many must run cleanly");

        peak_live = peak_live.max(s.resources.live_count());

        // --- prove it works: readback the marker + clear-render one texture and read a pixel ---
        let back = exec
            .read_buffer(&s.resources, BufferId(1), 0, marker.len())
            .unwrap();
        assert_eq!(
            back, marker,
            "cycle {cycle}: freshly-created buffer must round-trip its bytes"
        );
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.1, 0.2, 0.3, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            })],
        )
        .expect("cycle render must run cleanly");
        let img = exec.read_texture(&s.resources, 1).unwrap();
        assert!(
            near_tol(px(&img, 64, 0, 0), [26, 51, 77, 255], 3),
            "cycle {cycle}: fresh texture must clear"
        );

        // --- destroy many ---
        let mut destroy: Vec<Cmd> = Vec::with_capacity(T6_BUFS + T6_TEXS);
        for b in 0..T6_BUFS {
            destroy.push(Cmd::DestroyBuffer((b as u32) + 1));
        }
        for t in 0..T6_TEXS {
            destroy.push(Cmd::DestroyTexture((t as u32) + 1));
        }
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &destroy)
            .expect("destroy-many must run cleanly");

        // Leak gate: everything created this cycle is gone — live count, resident bytes, and the process
        // global account must all be back at the pre-cycle baseline. No drift permitted across cycles.
        assert_eq!(
            s.resources.live_count(),
            baseline_live,
            "cycle {cycle}: live objects did not return to baseline (leak)"
        );
        assert_eq!(
            s.account.ledger().residency_bytes(),
            baseline_bytes,
            "cycle {cycle}: resident bytes did not return to baseline (leak)"
        );
        assert_eq!(
            s.global.residency_bytes(),
            baseline_global,
            "cycle {cycle}: global residency did not return to baseline (leak)"
        );
        assert_eq!(
            s.global.object_count(),
            baseline_objs,
            "cycle {cycle}: global object count did not return to baseline (leak)"
        );
    }

    println!(
        "[scale] no_resource_leak: {T6_CYCLES} cycles × ({T6_BUFS} buffers @256KiB + {T6_TEXS} textures) — \
         peak {peak_live} live objects, returned to baseline ({baseline_live} live / {baseline_bytes} B) every cycle",
    );
    // Sanity: each cycle genuinely allocated the full batch (proving the baseline-return is meaningful).
    assert!(
        peak_live >= baseline_live + T6_BUFS + T6_TEXS,
        "cycles must have allocated the full resource batch"
    );
}
