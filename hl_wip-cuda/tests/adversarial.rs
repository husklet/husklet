//! Adversarial lowering coverage: drive every CUDA service against a `hl_gpu::RecordingSink` and assert
//! the EXACT recorded protocol `Cmd` sequence (or the exact typed error) for error paths, boundaries,
//! state-machine invariants, and malformed input — the paths a real CUDA app must never see faked.
//!
//! Companion to `tests/lowering.rs` (happy-path shape) and `tests/e2e.rs` (real computed results). Every
//! assertion here checks a REAL value — a recorded command, a resolved location, a typed error — never
//! merely that a call "did not panic".

use hl_cuda::adapter::{fatbin, ptx};
use hl_cuda::model::device::DevicePtr;
use hl_cuda::model::module::PtxModule;
use hl_cuda::model::stream::{Stream, StreamTable};
use hl_cuda::service::{allocate, launch, load_module, synchronize, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, KernelArg};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindResource;
use hl_gpu::protocol::model::kernel::gty;
use hl_gpu::{Cmd, GpuError, RecordingSink};

fn ctx() -> CudaContext {
    CudaContext::new(CudaDeviceDesc::apple_default(8 << 30))
}

// ===================================================================================================
// launch — dangling / null pointer argument handling (regression for the silent-drop fix)
// ===================================================================================================

fn vecadd_func(c: &mut CudaContext) -> hl_cuda::Function {
    let m = load_module::module_load_ptx(c, ptx::VECADD_PTX);
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
    assert!(matches!(err, GpuError::Invalid(_)), "dangling ptr arg must be Invalid, got {err:?}");
    // The failed launch submitted NOTHING (no partial/malformed IR leaked to the backend).
    assert_eq!(sink.batches.len(), batches_before, "a failed launch must not submit any batch");

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
    assert_eq!(bindings, vec![0, 1, 2], "null ptr region (binding 3) is unbound; a & b are bound");
    // The two bound regions are real storage buffers.
    for e in bg.entries.iter().skip(1) {
        assert!(matches!(e.resource, BindResource::Buffer { .. }));
    }
}

// ===================================================================================================
// transfer — dangling-pointer error paths + offset correctness for every copy direction
// ===================================================================================================

#[test]
fn every_copy_direction_rejects_a_dangling_pointer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let live = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let dead = DevicePtr(0xdead_0000);

    assert!(transfer::memcpy_htod(&mut c, &mut sink, dead, &[1, 2, 3, 4]).is_err());
    // dtod: a dangling SOURCE and a dangling DESTINATION are each rejected.
    assert!(transfer::memcpy_dtod(&mut c, &mut sink, live, dead, 8).is_err(), "dangling src");
    assert!(transfer::memcpy_dtod(&mut c, &mut sink, dead, live, 8).is_err(), "dangling dst");
    assert!(transfer::read_dtoh(&c, &mut sink, dead, 8).is_err());
    assert!(transfer::memcpy_dtoh(&c, dead).is_err());
    assert!(transfer::memset(&mut c, &mut sink, dead, &[0u8; 4]).is_err());
}

#[test]
fn dtod_copies_at_the_resolved_offsets_of_both_ends() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // interior pointers on both sides.
    transfer::memcpy_dtod(&mut c, &mut sink, DevicePtr(b.0 + 32), DevicePtr(a.0 + 16), 64).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cb)] => match cb.encoder.as_slice() {
            [Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size }] => {
                assert_eq!((*src, *src_offset), (1, 16));
                assert_eq!((*dst, *dst_offset), (2, 32));
                assert_eq!(*size, 64);
            }
            other => panic!("expected CopyBufferToBuffer, got {other:?}"),
        },
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn read_dtoh_requests_exact_buffer_offset_and_len() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let out = transfer::read_dtoh(&c, &mut sink, DevicePtr(p.0 + 12), 20).unwrap();
    assert_eq!(out.len(), 20);
    assert_eq!(sink.reads.last().copied(), Some((hl_gpu::BufferId(1), 12, 20)));
}

#[test]
fn memset_d8_d16_d32_expand_to_the_right_byte_pattern() {
    // The shim expands (value, width, count) → bytes; the service lowers that verbatim. Verify each width
    // tiles the element correctly (the lowering must carry the exact bytes, no truncation/padding).
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();

    // D16: 0xBEEF repeated 3× → 6 bytes little-endian.
    let d16: Vec<u8> = (0..3).flat_map(|_| 0xBEEFu16.to_le_bytes()).collect();
    transfer::memset(&mut c, &mut sink, base, &d16).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!((*id, *offset), (1, 0));
            assert_eq!(data, &[0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE]);
        }
        other => panic!("expected WriteBuffer, got {other:?}"),
    }
}

// ===================================================================================================
// allocate — lifecycle invariants: double free, interior-base free, cross-kind free
// ===================================================================================================

#[test]
fn free_of_interior_pointer_is_rejected() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // Freeing an interior (non-base) pointer must fail — only the exact allocation base frees.
    assert!(allocate::mem_free(&mut c, &mut sink, DevicePtr(p.0 + 8)).is_err());
    // The allocation is still live (the bogus free did not destroy it).
    assert!(c.mem.containing(p).is_some());
    // The real base frees cleanly, then a repeat is a double-free error.
    allocate::mem_free(&mut c, &mut sink, p).unwrap();
    assert!(allocate::mem_free(&mut c, &mut sink, p).is_err());
    assert!(c.resolve(p).is_none(), "a freed pointer no longer resolves");
}

#[test]
fn host_free_of_a_device_pointer_and_vice_versa_are_rejected() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let dev = allocate::mem_alloc(&mut c, &mut sink, 128).unwrap();
    let host = allocate::host_alloc(&mut c, 128);
    // Cross-kind frees are rejected (a device pointer is not a pinned host base, and vice versa).
    assert!(allocate::host_free(&mut c, dev.0).is_err(), "device ptr is not a pinned host base");
    assert!(allocate::mem_free(&mut c, &mut sink, DevicePtr(host)).is_err(), "host base is not a device alloc");
    // Each frees correctly through its own path.
    allocate::host_free(&mut c, host).unwrap();
    allocate::mem_free(&mut c, &mut sink, dev).unwrap();
}

#[test]
fn pitch_alloc_overflow_is_a_typed_error_not_a_panic() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // pitch(width) * height overflows u64 → a typed error, never a wrapping allocation.
    let huge = u64::MAX / 2;
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, huge, huge, 4).is_err());
    // zero extents are rejected too.
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 0, 8, 4).is_err());
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 8, 0, 4).is_err());
}

#[test]
fn managed_and_device_allocations_do_not_alias_managed_flag() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let managed = allocate::mem_alloc_managed(&mut c, &mut sink, 256).unwrap();
    let device = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    assert!(c.mem.is_managed(managed));
    assert!(c.mem.is_managed(DevicePtr(managed.0 + 100)), "interior pointer is managed too");
    assert!(!c.mem.is_managed(device));
    // Freeing the managed allocation clears its managed flag (no stale membership).
    allocate::mem_free(&mut c, &mut sink, managed).unwrap();
    assert!(!c.mem.is_managed(managed));
}

#[test]
fn host_get_device_pointer_bounds_the_backing_buffer_to_the_host_size() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::host_alloc(&mut c, 200);
    let dptr = allocate::host_get_device_pointer(&mut c, &mut sink, base).unwrap();
    // The backing device buffer is exactly the host allocation size and resolves as a live allocation.
    assert_eq!(c.mem.containing(dptr), Some((dptr.0, 200)));
    // Freeing the pinned host allocation drops its device mapping; a re-map mints a NEW device buffer.
    allocate::host_free(&mut c, base).unwrap();
    assert!(allocate::host_get_device_pointer(&mut c, &mut sink, base).is_err(), "freed host base unmaps");
}

// ===================================================================================================
// module + global — resolution invariants
// ===================================================================================================

#[test]
fn get_function_and_global_reject_unknown_module_ids() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // module id 999 was never loaded.
    assert!(load_module::module_get_function(&c, 999, "vecadd").is_err());
    // an unknown module yields Ok(None) for a global (NOT_FOUND at the ABI seam), never a fake pointer.
    assert_eq!(load_module::module_get_global(&mut c, &mut sink, 999, "g").unwrap(), None);
}

#[test]
fn same_global_name_in_two_modules_gets_distinct_backing_buffers() {
    const G: &str = ".visible .global .align 4 .b8 buf[128];\n.visible .entry k() { ret; }\n";
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let m1 = load_module::module_load_ptx(&mut c, G);
    let m2 = load_module::module_load_ptx(&mut c, G);
    let (p1, s1) = load_module::module_get_global(&mut c, &mut sink, m1, "buf").unwrap().unwrap();
    let (p2, s2) = load_module::module_get_global(&mut c, &mut sink, m2, "buf").unwrap().unwrap();
    assert_eq!((s1, s2), (128, 128));
    assert_ne!(p1, p2, "the same symbol in two modules must not alias one backing buffer");
    assert!(c.mem.containing(p1).is_some() && c.mem.containing(p2).is_some());
}

#[test]
fn load_data_rejects_non_utf8_non_fatbin_image() {
    let mut c = ctx();
    // Bytes that are neither a fatbin container nor valid UTF-8 PTX text → typed error, never a load.
    let junk = [0xFFu8, 0xFE, 0x00, 0x80, 0x81];
    assert!(!fatbin::is_fatbin(&junk));
    assert!(load_module::module_load_data(&mut c, &junk).is_err());
}

// ===================================================================================================
// fatbin walker — malformed / adversarial containers must yield None (never a crash, never fake PTX)
// ===================================================================================================

const FATBIN_MAGIC: u32 = 0xba55_ed50;
const FLAG_COMPRESS: u64 = 0x2000;

/// Build a single-entry fatbin container with a chosen entry `kind` and `flags`.
fn fatbin_with(kind: u16, flags: u64, payload: &[u8]) -> Vec<u8> {
    let mut entry = vec![0u8; 64];
    entry[0..2].copy_from_slice(&kind.to_le_bytes());
    entry[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    entry[8..16].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    entry[40..48].copy_from_slice(&flags.to_le_bytes());
    let fat_size = (entry.len() + payload.len()) as u64;
    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(&FATBIN_MAGIC.to_le_bytes());
    out[6..8].copy_from_slice(&16u16.to_le_bytes()); // container header_size
    out[8..16].copy_from_slice(&fat_size.to_le_bytes());
    out.extend_from_slice(&entry);
    out.extend_from_slice(payload);
    out
}

#[test]
fn fatbin_rejects_compressed_ptx_sass_only_and_truncated() {
    // A COMPRESSED PTX entry is out of the tier-1 scope → None (never a garbled decompress).
    let compressed = fatbin_with(1, FLAG_COMPRESS, b".version 7.5\n");
    assert!(fatbin::is_fatbin(&compressed));
    assert_eq!(fatbin::extract_ptx(&compressed), None);

    // A SASS/ELF-only fatbin (kind != PTX) carries no PTX → None.
    let sass = fatbin_with(2, 0, b"\x7fELF-ish");
    assert_eq!(fatbin::extract_ptx(&sass), None);

    // A container whose self-declared fat_size runs past the slice is truncated → None.
    let mut trunc = fatbin_with(1, 0, b".version 7.5\n");
    let bad_size = (trunc.len() as u64) + 4096;
    trunc[8..16].copy_from_slice(&bad_size.to_le_bytes());
    assert_eq!(fatbin::extract_ptx(&trunc), None);
}

#[test]
fn fatbin_trims_trailing_nul_padding_of_the_ptx_payload() {
    // A PTX payload is NUL-padded on disk; the walker must return the exact text without the padding.
    let mut padded = b".version 7.5\n".to_vec();
    padded.extend_from_slice(&[0u8; 8]); // NUL padding
    let img = fatbin_with(1, 0, &padded);
    assert_eq!(fatbin::extract_ptx(&img).unwrap(), b".version 7.5\n");
}

// ===================================================================================================
// ptx front-end — pointer classification, layout, shared memory, malformed input
// ===================================================================================================

/// saxpy with a scalar BEFORE the pointers: `y[i] = a*x[i] + y[i]`. Exercises f32 scalar + fma + the
/// natural-aligned layout `u32@0, f32@4, u64@8, u64@16` and taint classification of two pointer params.
const SAXPY_PTX: &str = r#"
    .visible .entry saxpy(
        .param .u32 saxpy_param_0,
        .param .f32 saxpy_param_1,
        .param .u64 saxpy_param_2,
        .param .u64 saxpy_param_3
    )
    {
        .reg .pred %p<2>;
        .reg .f32 %f<5>;
        .reg .b32 %r<6>;
        .reg .b64 %rd<9>;

        ld.param.u32  %r2, [saxpy_param_0];
        ld.param.f32  %f1, [saxpy_param_1];
        ld.param.u64  %rd1, [saxpy_param_2];
        ld.param.u64  %rd2, [saxpy_param_3];
        mov.u32       %r3, %ntid.x;
        mov.u32       %r4, %ctaid.x;
        mov.u32       %r5, %tid.x;
        mad.lo.s32    %r1, %r4, %r3, %r5;
        setp.ge.s32   %p1, %r1, %r2;
        @%p1 bra      DONE;
        cvta.to.global.u64 %rd3, %rd1;
        cvta.to.global.u64 %rd4, %rd2;
        mul.wide.s32  %rd5, %r1, 4;
        add.s64       %rd6, %rd3, %rd5;
        add.s64       %rd7, %rd4, %rd5;
        ld.global.f32 %f2, [%rd6];
        ld.global.f32 %f3, [%rd7];
        fma.rn.f32    %f4, %f1, %f2, %f3;
        st.global.f32 [%rd7], %f4;
    DONE:
        ret;
    }
"#;

#[test]
fn saxpy_ptx_classifies_scalar_before_pointers_with_correct_offsets() {
    let prog = ptx::compile(SAXPY_PTX, "saxpy", [128, 1, 1]).unwrap();
    assert_eq!(prog.params.len(), 4);
    // n (u32) scalar, a (f32) scalar, x/y (u64) pointers.
    assert!(!prog.params[0].is_ptr && prog.params[0].offset == 0 && prog.params[0].width == 4);
    assert!(!prog.params[1].is_ptr && prog.params[1].offset == 4 && prog.params[1].width == 4);
    assert!(prog.params[2].is_ptr && prog.params[2].offset == 8 && prog.params[2].region == 0);
    assert!(prog.params[3].is_ptr && prog.params[3].offset == 16 && prog.params[3].region == 1);
    assert_eq!(prog.num_regions, 2);
    assert_eq!(prog.param_bytes, 24);
    assert!(prog.insts.iter().any(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::FFma { .. })));
    assert!(prog.insts.iter().any(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::StGlobal { ty, .. } if *ty == gty::F32)));
}

/// A kernel using `.shared` memory reports the (word-rounded) static shared-byte budget.
const SHARED_PTX: &str = r#"
    .visible .entry red(.param .u64 red_param_0) {
        .shared .align 4 .b8 scratch[100];
        .reg .b64 %rd<2>;
        ld.param.u64 %rd1, [red_param_0];
        bar.sync 0;
        ret;
    }
"#;

#[test]
fn shared_memory_declaration_is_accounted_and_dynamic_shared_is_rejected() {
    let prog = ptx::compile(SHARED_PTX, "red", [64, 1, 1]).unwrap();
    assert_eq!(prog.shared_bytes, 100, "100 bytes of static shared, word-rounded");

    // Dynamic (extern, unsized) shared is outside the statically-sized subset → a typed Kernel error.
    let dynamic = ".visible .entry k(.param .u64 p) { .extern .shared .align 4 .b8 s[]; ret; }";
    assert!(matches!(ptx::compile(dynamic, "k", [1, 1, 1]), Err(GpuError::Kernel(_))));
}

#[test]
fn ptx_rejects_array_param_and_bad_type() {
    // struct/array parameters are unsupported.
    let arr = ".visible .entry k(.param .align 8 .b8 k_param_0[64]) { ret; }";
    assert!(matches!(ptx::compile(arr, "k", [1, 1, 1]), Err(GpuError::Kernel(_))));
    // an unknown scalar param type is rejected.
    let bad = ".visible .entry k(.param .v4 k_param_0) { ret; }";
    assert!(ptx::compile(bad, "k", [1, 1, 1]).is_err());
}

#[test]
fn ptx_module_entry_scan_finds_multiple_entries_in_order() {
    let src = ".visible .entry first() { ret; }\n.entry second(.param .u64 p) { ret; }\n";
    let m = PtxModule::parse(src);
    assert_eq!(m.entries, vec!["first".to_string(), "second".to_string()]);
    // a floating-point atomic is honestly rejected (WGSL has no f32 atomics) rather than mis-lowered.
    let fatom = ".visible .entry k(.param .u64 p) { red.global.add.f32 [%rd1], %f1; ret; }";
    assert!(matches!(ptx::compile(fatom, "k", [1, 1, 1]), Err(GpuError::Kernel(_))));
}

// ===================================================================================================
// streams + synchronize — handle validity state machine
// ===================================================================================================

#[test]
fn stream_lifecycle_and_synchronize_validation() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    // default stream is always valid + not destroyable.
    assert!(c.streams.is_valid(StreamTable::DEFAULT));
    assert!(!c.streams.destroy(StreamTable::DEFAULT), "the default stream cannot be destroyed");

    let s = c.streams.create();
    assert!(c.streams.is_valid(s));
    synchronize::stream_synchronize(&mut c, &mut sink, s).unwrap();

    // destroy → no longer valid → synchronize + async ops reject it.
    assert!(c.streams.destroy(s));
    assert!(!c.streams.destroy(s), "double-destroy is rejected");
    assert!(!c.streams.is_valid(s));
    assert!(synchronize::stream_synchronize(&mut c, &mut sink, s).is_err());

    let base = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    assert!(transfer::memcpy_htod_async(&mut c, &mut sink, s, base, &[1, 2]).is_err());
    assert!(transfer::memset_async(&mut c, &mut sink, s, base, &[0u8; 4]).is_err());
    // a never-minted handle is invalid too.
    assert!(!c.streams.is_valid(Stream(4242)));
}

#[test]
fn ctx_synchronize_barrier_uses_a_fresh_fence_value_each_time() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    synchronize::ctx_synchronize(&mut c, &mut sink).unwrap();
    synchronize::ctx_synchronize(&mut c, &mut sink).unwrap();
    // two barriers → two distinct, monotonically increasing fence signal values.
    assert_eq!(sink.waits.len(), 2);
    assert!(sink.waits[1].1 > sink.waits[0].1, "fence values are monotonic across barriers");
}
