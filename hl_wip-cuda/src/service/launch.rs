//! `cuLaunchKernel` — the compute lowering: pack kernel args → the
//! `CreateShader`/`CreateComputePipeline`/`CreateBuffer`/`CreateBindGroup`/`Submit(Dispatch)` sequence.
//!
//! Ported byte-for-byte in behaviour from `hl-gpu/src/cuda.rs` (`CudaContext::launch`). The one shader
//! carried across the IR is a neutral **kernel descriptor** (PTX text + entry + block dims,
//! [`KernelDescriptor::to_words`]) — the host backend compiles it (software → kernel-IR + CPU
//! interpreter; Metal → PTX→SPIR-V→MSL). The shader+pipeline are cached by `(module, entry, block)` so a
//! repeat launch emits no new `CreateShader`/`CreateComputePipeline`.
//!
//! ## Kernel argument ABI (the seam the interpreter + Metal both honour)
//! * **binding 0** = the flat kernel-parameter blob, laid out exactly like CUDA's `cuLaunchKernel`
//!   parameter space (each argument at its natural-aligned offset). Scalars are read from here.
//! * **binding r+1** = the storage buffer for the `r`-th pointer argument (in argument order). A pointer
//!   parameter dereferences its own region — the per-allocation binding model Metal uses.

use crate::model::context::CudaContext;
use crate::model::module::{Function, KernelArg};
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{Cmd, CommandBuffer, CommandSink, GpuError, Result, ShaderPayloadKind};

/// `cuLaunchKernel(func, grid, block, args)` → the compute IR, submitted as one batch. `grid` = number
/// of blocks (→ workgroup count); `block` = threads per block (→ threadgroup size, baked into the
/// compiled kernel as the WebGPU/Metal `local_size`). Returns the compute pipeline id used.
pub fn launch(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    func: Function,
    grid: (u32, u32, u32),
    block: (u32, u32, u32),
    args: &[KernelArg],
) -> Result<u32> {
    let block_arr = [block.0, block.1, block.2];

    // Marshal the arguments FIRST — before minting/caching any shader or pipeline id. A dangling
    // (non-null) device-pointer argument is a hard error (the `CUDA_ERROR_INVALID_VALUE` analogue,
    // matching every `cuMemcpy*` path), not a silently-dropped binding that would leave the kernel's
    // storage region unbound (an unbound output region is discarded on writeback → a fake-success launch
    // that computed nothing). A NULL pointer (`0`) is a legal kernel argument and binds no region.
    // Validating up front also guarantees we never cache a pipeline whose `CreateShader`/`CreatePipeline`
    // never actually reached the backend (this function returns before `sink.submit`).
    let mut blob: Vec<u8> = Vec::new();
    let mut entries: Vec<BindEntry> = Vec::new();
    let mut region = 0u32;
    for a in args {
        match a {
            KernelArg::Ptr(p) => {
                // natural-align to 8 (pointer width), then store the device address in the blob.
                align_blob(&mut blob, 8);
                blob.extend_from_slice(&p.0.to_le_bytes());
                if p.0 != 0 {
                    let (buf, off) = ctx.resolve(*p).ok_or(GpuError::Invalid(
                        "cuLaunchKernel: kernel argument is a dangling device pointer",
                    ))?;
                    entries.push(BindEntry {
                        binding: region + 1,
                        resource: BindResource::Buffer { id: buf.0, offset: off, size: 0 },
                    });
                }
                region += 1;
            }
            KernelArg::Scalar(bytes) => {
                align_blob(&mut blob, bytes.len().max(1) as u64);
                blob.extend_from_slice(bytes);
            }
        }
    }

    let mut out: Vec<Cmd> = Vec::new();

    // lazily create the kernel shader (forwarded PTX descriptor) + compute pipeline.
    let pipeline = if let Some((_, pipeline)) = ctx.cached_pipeline(func.module, func.entry, block_arr) {
        pipeline
    } else {
        let shader = ctx.alloc_shader();
        let pipeline = ctx.alloc_pipeline();
        let (ptx_src, entry_name) = ctx.entry_source(func).unwrap_or_default();
        let desc = KernelDescriptor { ptx: ptx_src, entry: entry_name.clone(), block: block_arr };
        out.push(Cmd::CreateShader {
            id: shader,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: desc.to_words(),
        });
        out.push(Cmd::CreateComputePipeline(
            pipeline,
            ComputePipelineDesc {
                compute: ShaderRef { module: shader, entry: entry_name },
                label: String::new(),
            },
        ));
        ctx.cache_pipeline(func.module, func.entry, block_arr, (shader, pipeline));
        pipeline
    };

    // Materialize the parameter blob as a small uniform/storage buffer bound at binding 0.
    let param_buf = ctx.alloc_buffer();
    out.push(Cmd::CreateBuffer(
        param_buf,
        BufferDesc {
            size: blob.len().max(1) as u64,
            usage: buffer_usage::UNIFORM | buffer_usage::STORAGE | buffer_usage::COPY_DST,
            label: "kernel-params".into(),
        },
    ));
    if !blob.is_empty() {
        out.push(Cmd::WriteBuffer { id: param_buf, offset: 0, data: blob });
    }
    entries.insert(
        0,
        BindEntry { binding: 0, resource: BindResource::Buffer { id: param_buf, offset: 0, size: 0 } },
    );

    let bind_group = ctx.alloc_bind_group();
    out.push(Cmd::CreateBindGroup(bind_group, BindGroupDesc { set: 0, entries }));

    out.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(pipeline),
            Enc::SetBindGroup { index: 0, group: bind_group },
            Enc::Dispatch { x: grid.0, y: grid.1, z: grid.2 },
            Enc::EndComputePass,
        ],
        signal: None,
    }));
    // The parameter buffer and bind group are launch-local: the submit above is synchronous, so release
    // them right after so repeated launches don't grow the backend's resource tables without bound. The
    // cached shader/pipeline (keyed by entry+block) intentionally persist for reuse.
    out.push(Cmd::DestroyBindGroup(bind_group));
    out.push(Cmd::DestroyBuffer(param_buf));

    sink.submit(&out)?;
    Ok(pipeline)
}

/// Pad `blob` up to a natural-alignment boundary before appending the next kernel parameter.
fn align_blob(blob: &mut Vec<u8>, align: u64) {
    let a = align as usize;
    while blob.len() % a != 0 {
        blob.push(0);
    }
}
