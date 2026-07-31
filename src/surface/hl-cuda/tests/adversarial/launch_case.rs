use super::*;

// ===================================================================================================
// launch — dangling / null pointer argument handling (regression for the silent-drop fix)
// ===================================================================================================

fn vecadd_func(c: &mut CudaContext) -> hl_cuda::Function {
    let m = c.load_ptx(ptx::VECADD_PTX);
    load_module::module_get_function(c, m, "vecadd").unwrap()
}

/// A launch whose pointer argument is a freed/dangling device pointer is a hard `Invalid` error — never a
/// success that silently drops the storage binding (which would dispatch a kernel with an unbound region
/// and discard its output on writeback). Nothing is submitted, and the pipeline is NOT cached.
#[test]
fn launch_with_dangling_pointer_arg_errors_and_does_not_submit_or_cache() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let f = vecadd_func(&mut c);

    let a = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let out = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    // Free `out` so its pointer is now dangling — but still pass it as the third kernel arg.
    allocate::mem_free(&mut c, &mut sink, out).unwrap();

    let batches_before = sink.batches.len();
    let args = vec![
        KernelArg::Ptr(a),
        KernelArg::Ptr(b),
        KernelArg::Ptr(out), // dangling
        KernelArg::Scalar(16i32.to_le_bytes().to_vec()),
    ];
    let err = launch::launch(&mut c, &mut sink, f, (1, 1, 1), (16, 1, 1), &args).unwrap_err();
    assert!(
        matches!(err, GpuError::Invalid(_)),
        "dangling ptr arg must be Invalid, got {err:?}"
    );
    // The failed launch submitted NOTHING (no partial/malformed IR leaked to the backend).
    assert_eq!(
        sink.batches.len(),
        batches_before,
        "a failed launch must not submit any batch"
    );

    // And it did NOT poison the pipeline cache: a subsequent VALID launch of the same (module,entry,block)
    // must still create the shader + pipeline (a cached id whose CreateShader never reached the backend
    // would be a latent corruption).
    let out2 = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let good = vec![
        KernelArg::Ptr(a),
        KernelArg::Ptr(b),
        KernelArg::Ptr(out2),
        KernelArg::Scalar(16i32.to_le_bytes().to_vec()),
    ];
    launch::launch(&mut c, &mut sink, f, (1, 1, 1), (16, 1, 1), &good).unwrap();
    let batch = sink.batches.last().unwrap();
    assert!(
        matches!(batch[0], Cmd::CreateShader { .. }),
        "the valid launch must (re)create the shader — the cache was not poisoned"
    );
}

/// A NULL device pointer (`0`) is a legal kernel argument: it binds no storage region but the launch
/// still succeeds and every other region is bound at its correct binding index.
#[test]
fn launch_with_null_pointer_arg_binds_no_region_but_succeeds() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let f = vecadd_func(&mut c);
    let a = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();

    // Third pointer is NULL — a legal (if unusual) argument.
    let args = vec![
        KernelArg::Ptr(a),
        KernelArg::Ptr(b),
        KernelArg::Ptr(DevicePtr(0)),
        KernelArg::Scalar(16i32.to_le_bytes().to_vec()),
    ];
    launch::launch(&mut c, &mut sink, f, (1, 1, 1), (16, 1, 1), &args).unwrap();

    // Find the bind group: binding 0 (params) + region bindings 1 and 2 for `a` and `b` — but NOT a
    // binding for the null pointer's region (region index 2 → binding 3 is absent).
    let batch = sink.batches.last().unwrap();
    let bg = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateBindGroup(_, d) => Some(d),
            _ => None,
        })
        .expect("a bind group is emitted");
    let bindings: Vec<u32> = bg.entries.iter().map(|e| e.binding).collect();
    assert_eq!(
        bindings,
        vec![0, 1, 2],
        "null ptr region (binding 3) is unbound; a & b are bound"
    );
    // The two bound regions are real storage buffers.
    for e in bg.entries.iter().skip(1) {
        assert!(matches!(e.resource, BindResource::Buffer { .. }));
    }
}

// ===================================================================================================
// launch — a Function that does not resolve
// ===================================================================================================

/// A `Function` whose module or entry does not resolve is an error, never a dispatch of an empty kernel.
///
/// `Function` carries public `module` and `entry` fields, so the shim translating a guest's `CUfunction`
/// handle can present any pair of integers, and a fabricated or corrupted handle reaches `launch`
/// directly. `module_get_function` guards the front door, but nothing guards a `Function` that is
/// already held.
///
/// The failure this pins is silent rather than loud. `entry_source` returns `None` for an unresolvable
/// function, and taking the type's default on that path yields `("", "")` — an empty PTX source and an
/// empty entry name — which is then packed into a `KernelDescriptor`, submitted as `CreateShader` plus
/// `CreateComputePipeline`, cached, and dispatched. The kernel writes nothing, so the output buffer keeps
/// whatever it held and the guest reads back zeros. "Could not resolve the kernel" and "the kernel ran
/// and produced zeros" become indistinguishable at the only place a caller can observe them, which turns
/// a lookup failure into what looks like a compute bug.
///
/// `launch` already returns `Invalid` for a dangling pointer argument, so refusing here is its existing
/// contract rather than a new policy.
#[test]
fn launch_with_unresolvable_function_errors_and_does_not_submit_or_cache() {
    for label in ["unknown module", "entry past the end"] {
        let mut c = ctx();
        let m = c.load_ptx(ptx::VECADD_PTX);
        let bad = if label == "unknown module" {
            hl_cuda::Function { module: 999, entry: 0 }
        } else {
            hl_cuda::Function { module: m, entry: 999 }
        };

        let mut sink = RecordingSink::with_full_caps();
        let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
        let args = [KernelArg::Ptr(p), KernelArg::Ptr(p), KernelArg::Ptr(p)];
        let before = sink.commands().count();

        let outcome = launch::launch(&mut c, &mut sink, bad, (1, 1, 1), (16, 1, 1), &args);

        assert!(
            outcome.is_err(),
            "{label}: launching an unresolvable Function returned {outcome:?}; an empty kernel was \
             dispatched instead of the lookup failure being reported, so the guest would read back \
             zeros and see a compute bug",
        );
        // Nothing may be submitted for a launch that cannot resolve, and in particular no shader or
        // pipeline built from an empty descriptor may be left in the cache for a later launch to reuse.
        assert_eq!(
            sink.commands().count(),
            before,
            "{label}: commands were submitted for a launch that could not resolve its function",
        );
        // Nor may the refused launch consume identifiers. An id taken by a call that then failed is
        // the shape that wedges a session on retry, so a launch that resolves nothing must take
        // nothing: the next successful launch gets the ids the refused one would have had.
        let mut fresh = ctx();
        let fm = fresh.load_ptx(ptx::VECADD_PTX);
        let good = load_module::module_get_function(&fresh, fm, "vecadd").unwrap();
        let mut fresh_sink = RecordingSink::with_full_caps();
        let fp = allocate::mem_alloc(&mut fresh, &mut fresh_sink, 64).unwrap();
        let fargs = [KernelArg::Ptr(fp), KernelArg::Ptr(fp), KernelArg::Ptr(fp)];
        launch::launch(&mut fresh, &mut fresh_sink, good, (1, 1, 1), (16, 1, 1), &fargs).unwrap();

        let after_refusal = load_module::module_get_function(&c, m, "vecadd").unwrap();
        launch::launch(&mut c, &mut sink, after_refusal, (1, 1, 1), (16, 1, 1), &args).unwrap();
        let shader_ids = |sink: &RecordingSink| -> Vec<u32> {
            sink.commands()
                .filter_map(|cmd| match cmd {
                    Cmd::CreateShader { id, .. } => Some(*id),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            shader_ids(&sink),
            shader_ids(&fresh_sink),
            "{label}: the refused launch consumed a shader id, so the successful launch after it got a \
             different id than the same launch would have got on a fresh context",
        );
    }
}
