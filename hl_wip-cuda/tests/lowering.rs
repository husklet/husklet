//! Lowering tests: drive each CUDA service against a `hl_gpu::RecordingSink` and assert the exact
//! protocol `Cmd` sequence the operation lowers to (plus the PTX parser + fatbin walker adapters).
//!
//! This is the acceptance gate for the CUDA→IR lowering layer: no socket, no GPU — just the recorded
//! command stream, which is wire-identical to what the shipping system emits.

use hl_cuda::adapter::{fatbin, ptx};
use hl_cuda::model::stream::Stream;
use hl_cuda::result;
use hl_cuda::service::{allocate, launch, load_module, synchronize, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, KernelArg};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{BindResource, BufferDesc};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::{gty, Inst, KernelDescriptor};
use hl_gpu::{Cmd, GpuError, RecordingSink, ShaderPayloadKind};

fn ctx() -> CudaContext {
    CudaContext::new(CudaDeviceDesc::apple_default(8 << 30))
}

// ---------------------------------------------------------------------------------------------------
// allocate
// ---------------------------------------------------------------------------------------------------

#[test]
fn mem_alloc_emits_create_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 4096).unwrap();

    assert_eq!(sink.batches.len(), 1);
    match &sink.batches[0][0] {
        Cmd::CreateBuffer(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!(
                *desc,
                BufferDesc {
                    size: 4096,
                    usage: buffer_usage::STORAGE
                        | buffer_usage::COPY_SRC
                        | buffer_usage::COPY_DST
                        | buffer_usage::MAP,
                    label: String::new(),
                }
            );
        }
        other => panic!("expected CreateBuffer, got {other:?}"),
    }
    // device pointer is well above zero and 256-aligned.
    assert_eq!(p.0 % 256, 0);
    assert!(p.0 >= 0x10_0000);
}

#[test]
fn second_alloc_gets_distinct_buffer_and_bumped_pointer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 100).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 100).unwrap();
    assert_ne!(a.0, b.0);
    assert!(b.0 > a.0);
    // buffer ids 1 then 2
    assert!(matches!(sink.batches[0][0], Cmd::CreateBuffer(1, _)));
    assert!(matches!(sink.batches[1][0], Cmd::CreateBuffer(2, _)));
}

#[test]
fn mem_free_emits_destroy_buffer_and_rejects_bogus() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    allocate::mem_free(&mut c, &mut sink, p).unwrap();
    assert!(matches!(sink.batches.last().unwrap()[0], Cmd::DestroyBuffer(1)));

    // freeing again (or a bogus pointer) is a typed error, not a panic.
    let err = allocate::mem_free(&mut c, &mut sink, p).unwrap_err();
    assert!(matches!(err, GpuError::Invalid(_)));
}

#[test]
fn allocation_metadata_backs_pointer_and_mem_info_queries() {
    // The model data the driver's `cuPointerGetAttribute` / `cuMemGetAddressRange` / `cuMemGetInfo`
    // entry points read: `containing` resolves an interior pointer to its (base, size), and
    // `total_bytes` is the used figure `cuMemGetInfo` subtracts from total VRAM.
    use hl_cuda::model::device::DevicePtr;
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    assert_eq!(c.mem.total_bytes(), 0, "no allocations → nothing used");
    let a = allocate::mem_alloc(&mut c, &mut sink, 4096).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    assert_eq!(c.mem.total_bytes(), 4096 + 256, "used = sum of live allocation sizes");

    // An interior pointer resolves to the allocation's base + size.
    assert_eq!(c.mem.containing(DevicePtr(a.0 + 8)), Some((a.0, 4096)));
    assert_eq!(c.mem.containing(DevicePtr(b.0)), Some((b.0, 256)));
    // A dangling pointer resolves to nothing (→ CUDA_ERROR_INVALID_VALUE at the ABI seam).
    assert_eq!(c.mem.containing(DevicePtr(0xdead_beef)), None);

    // Free drops it from both the resolver and the used-bytes total (what cuMemGetInfo reflects).
    allocate::mem_free(&mut c, &mut sink, a).unwrap();
    assert_eq!(c.mem.total_bytes(), 256);
    assert_eq!(c.mem.containing(DevicePtr(a.0 + 8)), None);

    // The free/total cuMemGetInfo would report.
    let total = c.device.total_mem;
    let free = total - c.mem.total_bytes();
    assert_eq!(free, total - 256);
}

// ---------------------------------------------------------------------------------------------------
// transfer
// ---------------------------------------------------------------------------------------------------

#[test]
fn htod_writes_at_resolved_offset() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // write into the middle of the allocation via an offset device pointer.
    let dst = hl_cuda::DevicePtr(base.0 + 16);
    transfer::memcpy_htod(&mut c, &mut sink, dst, &[1, 2, 3, 4]).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!(*id, 1);
            assert_eq!(*offset, 16);
            assert_eq!(data, &[1, 2, 3, 4]);
        }
        other => panic!("expected one WriteBuffer, got {other:?}"),
    }
}

#[test]
fn dtod_emits_copy_buffer_to_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    transfer::memcpy_dtod(&mut c, &mut sink, b, a, 128).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cb)] => match cb.encoder.as_slice() {
            [Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size }] => {
                assert_eq!((*src, *src_offset), (1, 0));
                assert_eq!((*dst, *dst_offset), (2, 0));
                assert_eq!(*size, 128);
            }
            other => panic!("expected CopyBufferToBuffer, got {other:?}"),
        },
        other => panic!("expected one Submit, got {other:?}"),
    }
}

#[test]
fn dtoh_resolves_without_submitting() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let batches_before = sink.batches.len();
    let (buf, off) = transfer::memcpy_dtoh(&c, hl_cuda::DevicePtr(p.0 + 8)).unwrap();
    assert_eq!((buf.0, off), (1, 8));
    // no command was submitted for the readback.
    assert_eq!(sink.batches.len(), batches_before);
}

// ---------------------------------------------------------------------------------------------------
// load_module
// ---------------------------------------------------------------------------------------------------

#[test]
fn module_load_and_get_function() {
    let mut c = ctx();
    let m = load_module::module_load_ptx(&mut c, ptx::VECADD_PTX);
    let f = load_module::module_get_function(&c, m, "vecadd").unwrap();
    assert_eq!(f.module, m);
    assert_eq!(f.entry, 0);
    // an unknown entry is a typed error.
    assert!(load_module::module_get_function(&c, m, "nope").is_err());
}

/// Build a minimal single-entry uncompressed-PTX fatbin container around `ptx` bytes, matching the
/// layout the walker parses (16-byte container header + 64-byte entry header + payload).
fn build_fatbin(ptx: &[u8]) -> Vec<u8> {
    const FATBIN_MAGIC: u32 = 0xba55_ed50;
    let mut entry = vec![0u8; 64];
    entry[0..2].copy_from_slice(&1u16.to_le_bytes()); // kind = PTX
    entry[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    entry[8..16].copy_from_slice(&(ptx.len() as u64).to_le_bytes()); // payload_size
    // flags @40 = 0 (uncompressed)
    let fat_size = (entry.len() + ptx.len()) as u64;

    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(&FATBIN_MAGIC.to_le_bytes());
    out[6..8].copy_from_slice(&16u16.to_le_bytes()); // header_size
    out[8..16].copy_from_slice(&fat_size.to_le_bytes());
    out.extend_from_slice(&entry);
    out.extend_from_slice(ptx);
    out
}

#[test]
fn module_load_data_walks_fatbin() {
    let mut c = ctx();
    let image = build_fatbin(ptx::VECADD_PTX.as_bytes());
    assert!(fatbin::is_fatbin(&image));
    assert_eq!(fatbin::extract_ptx(&image).unwrap(), ptx::VECADD_PTX.as_bytes());

    let m = load_module::module_load_data(&mut c, &image).unwrap();
    assert!(load_module::module_get_function(&c, m, "vecadd").is_ok());
}

#[test]
fn module_load_data_accepts_raw_ptx_text() {
    let mut c = ctx();
    let m = load_module::module_load_data(&mut c, ptx::VECADD_PTX.as_bytes()).unwrap();
    assert!(load_module::module_get_function(&c, m, "vecadd").is_ok());
}

#[test]
fn fatbin_rejects_non_container() {
    assert!(!fatbin::is_fatbin(b"not a fatbin"));
    assert!(fatbin::extract_ptx(b"not a fatbin").is_none());
}

// ---------------------------------------------------------------------------------------------------
// launch — the core compute lowering
// ---------------------------------------------------------------------------------------------------

fn setup_vecadd(c: &mut CudaContext, sink: &mut RecordingSink) -> (hl_cuda::Function, Vec<KernelArg>) {
    let a = allocate::mem_alloc(c, sink, 1024).unwrap();
    let b = allocate::mem_alloc(c, sink, 1024).unwrap();
    let out = allocate::mem_alloc(c, sink, 1024).unwrap();
    let m = load_module::module_load_ptx(c, ptx::VECADD_PTX);
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
    assert_ne!(p1, p2, "different block dims bake a different kernel → new pipeline");
}

// ---------------------------------------------------------------------------------------------------
// synchronize
// ---------------------------------------------------------------------------------------------------

#[test]
fn ctx_synchronize_emits_fence_barrier() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    synchronize::ctx_synchronize(&mut c, &mut sink).unwrap();

    // batch 0: CreateFence + signalling Submit. batch 1: DestroyFence. one recorded wait.
    match sink.batches[0].as_slice() {
        [Cmd::CreateFence(fid), Cmd::Submit(cb)] => {
            assert_eq!(cb.signal, Some((*fid, 1)));
            assert!(cb.encoder.is_empty());
        }
        other => panic!("expected CreateFence + Submit, got {other:?}"),
    }
    assert!(matches!(sink.batches[1].as_slice(), [Cmd::DestroyFence(_)]));
    assert_eq!(sink.waits.len(), 1);
    assert_eq!(sink.waits[0].1, 1);
}

#[test]
fn stream_synchronize_validates_handle() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // the default stream is always valid.
    synchronize::stream_synchronize(&mut c, &mut sink, hl_cuda::model::stream::StreamTable::DEFAULT)
        .unwrap();
    // a created stream is valid.
    let s = c.streams.create();
    synchronize::stream_synchronize(&mut c, &mut sink, s).unwrap();
    // a bogus handle errors.
    let err = synchronize::stream_synchronize(&mut c, &mut sink, Stream(9999)).unwrap_err();
    assert!(matches!(err, GpuError::Invalid(_)));
}

// ---------------------------------------------------------------------------------------------------
// adapter::ptx — the parser
// ---------------------------------------------------------------------------------------------------

#[test]
fn vecadd_ptx_compiles_to_expected_program() {
    let prog = ptx::compile(ptx::VECADD_PTX, "vecadd", [256, 1, 1]).unwrap();
    assert_eq!(prog.entry, "vecadd");
    assert_eq!(prog.block, [256, 1, 1]);
    assert_eq!(prog.shared_bytes, 0);

    // 4 params: three device pointers (a, b, c) + one scalar (n). Pointer classification via the
    // forward taint pass over cvta/global accesses.
    assert_eq!(prog.params.len(), 4);
    assert_eq!(prog.num_regions, 3);
    assert!(prog.params[0].is_ptr && prog.params[0].region == 0);
    assert!(prog.params[1].is_ptr && prog.params[1].region == 1);
    assert!(prog.params[2].is_ptr && prog.params[2].region == 2);
    assert!(!prog.params[3].is_ptr);

    // natural-aligned flat layout: u64@0, u64@8, u64@16, u32@24 → 28-byte blob.
    assert_eq!(prog.params[0].offset, 0);
    assert_eq!(prog.params[1].offset, 8);
    assert_eq!(prog.params[2].offset, 16);
    assert_eq!(prog.params[3].offset, 24);
    assert_eq!(prog.param_bytes, 28);

    // the body ends in a ret, computes the global index (a mad), and does the elementwise f32 add/store.
    assert_eq!(prog.insts.last(), Some(&Inst::Ret));
    assert!(prog.insts.iter().any(|i| matches!(i, Inst::IMad { .. })));
    assert!(prog.insts.iter().any(|i| matches!(i, Inst::FAdd { .. })));
    assert!(prog
        .insts
        .iter()
        .any(|i| matches!(i, Inst::StGlobal { ty, .. } if *ty == gty::F32)));
    assert!(prog
        .insts
        .iter()
        .any(|i| matches!(i, Inst::LdGlobal { ty, .. } if *ty == gty::F32)));
}

#[test]
fn ptx_unknown_entry_errors() {
    assert!(matches!(
        ptx::compile(ptx::VECADD_PTX, "nope", [1, 1, 1]),
        Err(GpuError::Kernel(_))
    ));
}

#[test]
fn ptx_unsupported_opcode_errors() {
    let bad = ".visible .entry k() { shfl.sync.idx.b32 %r1, %r2, 0, 31, 0; ret; }";
    assert!(matches!(ptx::compile(bad, "k", [1, 1, 1]), Err(GpuError::Kernel(_))));
}

// ---------------------------------------------------------------------------------------------------
// result mapping
// ---------------------------------------------------------------------------------------------------

#[test]
fn gpu_error_maps_to_curesult() {
    assert_eq!(
        result::cu_result_from_gpu_error(&GpuError::Kernel("x".into())),
        result::CUDA_ERROR_INVALID_PTX
    );
    assert_eq!(
        result::cu_result_from_gpu_error(&GpuError::Unsupported("x")),
        result::CUDA_ERROR_NOT_SUPPORTED
    );
    assert_eq!(
        result::cu_result_from_gpu_error(&GpuError::Invalid("x")),
        result::CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        result::cudart_from_gpu_error(&GpuError::Kernel("x".into())),
        result::CUDART_ERROR_INVALID_PTX
    );
}
