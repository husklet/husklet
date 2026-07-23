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
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, KernelArg};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindResource;
use hl_gpu::protocol::model::kernel::gty;
use hl_gpu::{Cmd, GpuError, RecordingSink};

fn ctx() -> CudaContext {
    CudaContext::new(CudaDeviceDesc::apple_default(8 << 30))
}

#[path = "adversarial/allocation.rs"]
mod allocation;
#[path = "adversarial/fatbin_case.rs"]
mod fatbin_case;
#[path = "adversarial/launch_case.rs"]
mod launch_case;
#[path = "adversarial/module.rs"]
mod module;
#[path = "adversarial/ptx_case.rs"]
mod ptx_case;
#[path = "adversarial/streams.rs"]
mod streams;
#[path = "adversarial/transfer_case.rs"]
mod transfer_case;
