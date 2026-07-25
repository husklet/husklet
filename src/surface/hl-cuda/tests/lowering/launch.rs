use super::*;

// ---------------------------------------------------------------------------------------------------
// launch — the core compute lowering
// ---------------------------------------------------------------------------------------------------

fn setup_vecadd(
    c: &mut CudaContext,
    sink: &mut RecordingSink,
) -> (hl_cuda::Function, Vec<KernelArg>) {
    let a = allocate::mem_alloc(c, sink, 1024).unwrap();
    let b = allocate::mem_alloc(c, sink, 1024).unwrap();
    let out = allocate::mem_alloc(c, sink, 1024).unwrap();
    let m = c.load_ptx(ptx::VECADD_PTX);
    let f = load_module::module_get_function(c, m, "vecadd").unwrap();
    let args = vec![
        KernelArg::Ptr(a),
        KernelArg::Ptr(b),
        KernelArg::Ptr(out),
        KernelArg::Scalar(256i32.to_le_bytes().to_vec()),
    ];
    (f, args)
}

#[test]
fn launch_emits_full_compute_sequence() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let (f, args) = setup_vecadd(&mut c, &mut sink);

    launch::launch(&mut c, &mut sink, f, (1, 1, 1), (256, 1, 1), &args).unwrap();
    let batch = sink.batches.last().unwrap();

    // CreateShader (PtxKernel descriptor) → CreateComputePipeline → CreateBuffer(params) →
    // WriteBuffer(params) → CreateBindGroup → Submit(dispatch) → DestroyBindGroup → DestroyBuffer.
    assert_eq!(batch.len(), 8, "batch = {batch:#?}");

    // 1. shader carries a neutral PTX kernel descriptor that round-trips to the source + entry + block.
    match &batch[0] {
        Cmd::CreateShader { kind, spirv, .. } => {
            assert_eq!(*kind, ShaderPayloadKind::PtxKernel);
            let d = KernelDescriptor::from_words(spirv).unwrap().unwrap();
            assert_eq!(d.entry, "vecadd");
            assert_eq!(d.block, [256, 1, 1]);
            assert_eq!(d.ptx, ptx::VECADD_PTX);
        }
        other => panic!("expected CreateShader, got {other:?}"),
    }
    assert!(matches!(batch[1], Cmd::CreateComputePipeline(..)));
    assert!(matches!(batch[2], Cmd::CreateBuffer(..)));
    assert!(matches!(batch[3], Cmd::WriteBuffer { .. }));

    // 5. bind group: binding 0 = param blob, bindings 1..=3 = the three pointer regions.
    match &batch[4] {
        Cmd::CreateBindGroup(_, desc) => {
            assert_eq!(desc.set, 0);
            assert_eq!(desc.entries.len(), 4);
            assert_eq!(desc.entries[0].binding, 0);
            for (i, e) in desc.entries.iter().enumerate().skip(1) {
                assert_eq!(e.binding, i as u32);
                assert!(matches!(e.resource, BindResource::Buffer { .. }));
            }
        }
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }

    // 6. the dispatch command buffer.
    match &batch[5] {
        Cmd::Submit(cb) => {
            assert_eq!(
                cb.encoder,
                vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ]
            );
        }
        other => panic!("expected Submit, got {other:?}"),
    }
    assert!(matches!(batch[6], Cmd::DestroyBindGroup(_)));
    assert!(matches!(batch[7], Cmd::DestroyBuffer(_)));
}

#[test]
fn repeat_launch_reuses_pipeline() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let (f, args) = setup_vecadd(&mut c, &mut sink);

    let p1 = launch::launch(&mut c, &mut sink, f, (1, 1, 1), (256, 1, 1), &args).unwrap();
    let p2 = launch::launch(&mut c, &mut sink, f, (1, 1, 1), (256, 1, 1), &args).unwrap();
    assert_eq!(p1, p2, "same (module,entry,block) reuses the pipeline");

    // the second launch emits NO CreateShader / CreateComputePipeline — 6 commands, starting at the
    // parameter buffer.
    let batch = sink.batches.last().unwrap();
    assert_eq!(batch.len(), 6, "batch = {batch:#?}");
    assert!(matches!(batch[0], Cmd::CreateBuffer(..)));
    assert!(!batch.iter().any(|c| matches!(c, Cmd::CreateShader { .. })));
}

#[test]
fn launch_with_different_block_makes_new_pipeline() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let (f, args) = setup_vecadd(&mut c, &mut sink);
    let p1 = launch::launch(&mut c, &mut sink, f, (1, 1, 1), (256, 1, 1), &args).unwrap();
    let p2 = launch::launch(&mut c, &mut sink, f, (1, 1, 1), (128, 1, 1), &args).unwrap();
    assert_ne!(
        p1, p2,
        "different block dims bake a different kernel → new pipeline"
    );
}
