//! The values + invariants the protocol owns. Pure data — no serialization (see [`super::codec`]) and
//! no platform types (no cuda/vulkan/gl/wgpu/Metal/fd/IOSurface/DRM anywhere).

pub mod capability;
pub mod command;
pub mod descriptor;
pub mod enums;
pub mod error;
pub mod half;
pub mod id;
pub mod kernel;
