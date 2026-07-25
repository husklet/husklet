//! Hand-written CUDA Driver API entry points, grouped by ABI capability.

use core::ffi::{c_char, c_void};

use hl_cuda::adapter::ptx;
use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::*;
use hl_cuda::service::{allocate, launch as launch_service, load_module, transfer};
use hl_cuda::KernelArg;

use crate::state::ShimState;
use platform::{write_cstr, CInput};

mod device;
mod extended;
mod launch;
mod memory;
mod module;
mod platform;
mod resolve;
mod sync;

pub use device::*;
pub use extended::*;
pub use launch::*;
pub use memory::*;
pub use module::*;
pub use platform::*;
pub use resolve::*;
pub use sync::*;

#[cfg(test)]
mod tests;
