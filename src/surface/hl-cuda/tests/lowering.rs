//! Lowering tests: drive each CUDA service against a `hl_gpu::RecordingSink` and assert the exact
//! protocol `Cmd` sequence the operation lowers to (plus the PTX parser + fatbin walker adapters).
//!
//! This is the acceptance gate for the CUDA→IR lowering layer: no socket, no GPU — just the recorded
//! command stream, which is wire-identical to what the shipping system emits.

use hl_cuda::adapter::{fatbin, ptx};
use hl_cuda::model::stream::Stream;
use hl_cuda::result;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, KernelArg};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{BindResource, BufferDesc};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::{gty, Inst, KernelDescriptor};
use hl_gpu::{Cmd, GpuError, RecordingSink, ShaderPayloadKind};

fn ctx() -> CudaContext {
    CudaContext::new(CudaDeviceDesc::apple_default(8 << 30))
}

#[path = "lowering/allocation.rs"]
mod allocation;
#[path = "lowering/transfer.rs"]
mod copies;
#[path = "lowering/launch.rs"]
mod execution;
#[path = "lowering/memory.rs"]
mod memory;
#[path = "lowering/module.rs"]
mod module;
#[path = "lowering/ptx.rs"]
mod parser;
#[path = "lowering/result.rs"]
mod status;
#[path = "lowering/synchronize.rs"]
mod synchronize;
