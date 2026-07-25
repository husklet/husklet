//! Unsupported Vulkan extension entry points grouped by capability.
//!
//! These extensions are not advertised. Their entry points remain resolvable and return
//! truthful unsupported results without exposing partially implemented behavior.

/// Truthful result for a command from an extension this ICD does not advertise.
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;

/// Truthful result for a core command requiring an optional feature this ICD does not expose.
const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;

mod advanced;
mod command;
mod command_state;
mod descriptor;
mod device;
mod diagnostic;
mod display;
mod drawing;
mod interop;
mod lifecycle;
mod micromap;
mod optical;
mod presentation;
mod ray;
mod shader;
mod surface;
mod video;

pub use advanced::*;
pub use command::*;
pub use command_state::*;
pub use descriptor::*;
pub use device::*;
pub use diagnostic::*;
pub use display::*;
pub use drawing::*;
pub use interop::*;
pub use lifecycle::*;
pub use micromap::*;
pub use optical::*;
pub use presentation::*;
pub use ray::*;
pub use shader::*;
pub use surface::*;
pub use video::*;
