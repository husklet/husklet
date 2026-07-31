//! Command-buffer recording and Vulkan `vkCmd*` lowering.
//!
//! The public surface remains flat for the shim and tests. Implementation modules group commands by the
//! state and resources they own, while queue submission remains in [`super::submit`].

use crate::model::command::{CmdBufRec, CommandBufferState};
use crate::model::sync::DeferredOp;
use crate::*;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferBinding, ColorAttachment, DepthAttachment,
    Extent3d, Origin3d, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, IndexFormat, LoadOp, TextureAspect, TextureFormat,
};
use hl_gpu::{Cmd, CommandSink, GpuError, Result};
use std::collections::HashMap;

mod binding;
mod buffer;
mod execute;
mod image;
mod indirect;
mod render;
mod state;
mod sync;
mod write;

pub use binding::*;
pub use buffer::*;
pub use execute::*;
pub use image::*;
pub use indirect::*;
pub use render::*;
pub use state::*;
pub use sync::*;
pub use write::*;
