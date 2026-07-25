//! Resource creation — the `vkCreate*` / `vkAllocate*` lowering.
//!
//! Creation is grouped by the Vulkan object lifecycle it owns. This module remains the stable façade so
//! callers retain the existing `service::create::*` API.

mod descriptor;
mod pipeline;
mod resource;

pub use descriptor::*;
pub use pipeline::*;
pub use resource::*;
