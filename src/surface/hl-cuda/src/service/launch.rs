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
    hl_log::hl_debug!(
        hl_log::tag::CUDA,
        "launch mod={} entry={} grid={:?} block={:?} args={}",
        func.module,
        func.entry,
        grid,
        block,
        args.len()
    );
    hl_log::hl_count!(hl_log::tag::CUDA, "launches");
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "launch");

    // Validate the launch configuration against the modeled device BEFORE minting/caching any shader,
    // pipeline, or parameter buffer — an out-of-range grid/block is `CUDA_ERROR_INVALID_VALUE` in a real
    // driver, NOT a silently-accepted launch that the software oracle then happily runs (which would be a
    // fake-success: the model would compute a result for a configuration hardware could never dispatch).
    validate_launch_dims(ctx, grid, block)?;

    let block_arr = [block.0, block.1, block.2];

    // Marshal the arguments FIRST — before minting/caching any shader or pipeline id. A dangling
    // (non-null) device-pointer argument is a hard error (the `CUDA_ERROR_INVALID_VALUE` analogue,
    // matching every `cuMemcpy*` path), not a silently-dropped binding that would leave the kernel's
    // storage region unbound (an unbound output region is discarded on writeback → a fake-success launch
    // that computed nothing). A NULL pointer (`0`) is a legal kernel argument and binds no region.
    // Validating up front also guarantees we never cache a pipeline whose `CreateShader`/`CreatePipeline`
    // never actually reached the backend (this function returns before `sink.submit`).
    let mut blob = ParameterBlob::default();
    let mut entries: Vec<BindEntry> = Vec::new();
    let mut region = 0u32;
    for a in args {
        match a {
            KernelArg::Ptr(p) => {
                // natural-align to 8 (pointer width), then store the device address in the blob.
                blob.align(8);
                blob.extend(&p.0.to_le_bytes());
                if p.0 != 0 {
                    let (buf, off) = ctx.resolve(*p).ok_or_else(|| {
                        hl_log::hl_warn!(hl_log::tag::CUDA, "launch dangling arg ptr={:#x}", p.0);
                        GpuError::Invalid(
                            "cuLaunchKernel: kernel argument is a dangling device pointer",
                        )
                    })?;
                    entries.push(BindEntry {
                        binding: region + 1,
                        resource: BindResource::Buffer {
                            id: buf.0,
                            offset: off,
                            size: 0,
                        },
                    });
                }
                region += 1;
            }
            KernelArg::Scalar(bytes) => {
                blob.align(bytes.len().max(1));
                blob.extend(bytes);
            }
        }
    }

    let mut out: Vec<Cmd> = Vec::new();

    // lazily create the kernel shader (forwarded PTX descriptor) + compute pipeline.
    let pipeline =
        if let Some((_, pipeline)) = ctx.cached_pipeline(func.module, func.entry, block_arr) {
            pipeline
        } else {
            let shader = ctx.alloc_shader();
            let pipeline = ctx.alloc_pipeline();
            let (ptx_src, entry_name) = ctx.entry_source(func).unwrap_or_default();
            let desc = KernelDescriptor {
                ptx: ptx_src,
                entry: entry_name.clone(),
                block: block_arr,
            };
            out.push(Cmd::CreateShader {
                id: shader,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: desc.to_words(),
            });
            out.push(Cmd::CreateComputePipeline(
                pipeline,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: shader,
                        entry: entry_name,
                    },
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
        out.push(Cmd::WriteBuffer {
            id: param_buf,
            offset: 0,
            data: blob.into_vec(),
        });
    }
    entries.insert(
        0,
        BindEntry {
            binding: 0,
            resource: BindResource::Buffer {
                id: param_buf,
                offset: 0,
                size: 0,
            },
        },
    );

    let bind_group = ctx.alloc_bind_group();
    out.push(Cmd::CreateBindGroup(
        bind_group,
        BindGroupDesc { set: 0, entries },
    ));

    out.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(pipeline),
            Enc::SetBindGroup {
                index: 0,
                group: bind_group,
            },
            Enc::Dispatch {
                x: grid.0,
                y: grid.1,
                z: grid.2,
            },
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

/// Validate a `cuLaunchKernel` grid/block against the modeled device limits, returning the
/// `CUDA_ERROR_INVALID_VALUE` analogue ([`GpuError::Invalid`]) for a configuration a real driver rejects:
///
/// * a **zero** extent on any grid or block axis (CUDA requires every dim ≥ 1), and
/// * a block whose total thread count (`block.x * block.y * block.z`) exceeds the device's
///   `maxThreadsPerBlock` (1024 on the modeled Ampere-class device) — the exact
///   `cudaErrorInvalidConfiguration`/`CUDA_ERROR_INVALID_VALUE` a real `cuLaunchKernel` returns.
///
/// This is a hard precondition: it runs before any `Cmd` is built, so an invalid launch surfaces an
/// honest error and emits NOTHING to the sink.
/// The per-axis launch geometry limits the modeled device advertises through
/// `CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_*` / `MAX_GRID_DIM_*` and `cudaDeviceProp`.
const MAX_BLOCK_DIM: [u32; 3] = [1024, 1024, 64];
const MAX_GRID_DIM: [u32; 3] = [2147483647, 65535, 65535];

fn validate_launch_dims(
    ctx: &CudaContext,
    grid: (u32, u32, u32),
    block: (u32, u32, u32),
) -> Result<()> {
    if grid.0 == 0 || grid.1 == 0 || grid.2 == 0 {
        hl_log::hl_warn!(hl_log::tag::CUDA, "launch zero grid dim {:?}", grid);
        return Err(GpuError::Invalid("cuLaunchKernel: grid dimension is zero"));
    }
    if block.0 == 0 || block.1 == 0 || block.2 == 0 {
        hl_log::hl_warn!(hl_log::tag::CUDA, "launch zero block dim {:?}", block);
        return Err(GpuError::Invalid("cuLaunchKernel: block dimension is zero"));
    }
    // Per-axis limits must match what `cuDeviceGetAttribute`/`cudaDeviceProp` advertise, or the driver
    // would accept a grid/block the device it describes could never dispatch.
    if block.0 > MAX_BLOCK_DIM[0] || block.1 > MAX_BLOCK_DIM[1] || block.2 > MAX_BLOCK_DIM[2] {
        return Err(GpuError::Invalid(
            "cuLaunchKernel: block dimension exceeds device maxThreadsDim",
        ));
    }
    if grid.0 > MAX_GRID_DIM[0] || grid.1 > MAX_GRID_DIM[1] || grid.2 > MAX_GRID_DIM[2] {
        return Err(GpuError::Invalid(
            "cuLaunchKernel: grid dimension exceeds device maxGridSize",
        ));
    }
    // Thread-count product in u64 so a `u32^3` block can never overflow past the comparison.
    let threads = (block.0 as u64) * (block.1 as u64) * (block.2 as u64);
    if threads > ctx.device.max_threads_per_block as u64 {
        hl_log::hl_warn!(
            hl_log::tag::CUDA,
            "launch block {:?} = {} threads > maxThreadsPerBlock {}",
            block,
            threads,
            ctx.device.max_threads_per_block
        );
        return Err(GpuError::Invalid(
            "cuLaunchKernel: threads per block exceeds device maxThreadsPerBlock",
        ));
    }
    Ok(())
}

/// Pad `blob` up to a natural-alignment boundary before appending the next kernel parameter.
#[derive(Default)]
struct ParameterBlob(Vec<u8>);

impl ParameterBlob {
    fn align(&mut self, alignment: usize) {
        while !self.0.len().is_multiple_of(alignment) {
            self.0.push(0);
        }
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn into_vec(self) -> Vec<u8> {
        self.0
    }
}
