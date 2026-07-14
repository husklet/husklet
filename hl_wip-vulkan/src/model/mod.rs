//! The Vulkan object model + its invariants (OVERVIEW-v2 §2 `model/`).
//!
//! Pure owned values: no `Cmd` is built here and nothing is submitted. A [`device::Device`] aggregates
//! the per-device state (the physical-device descriptor, the resource handle tables, the id counters);
//! the [`super::service`] layer drives it and emits the IR. The object model + reported device props
//! are ported from `hl-shim-vk/src/{state.rs,reg.rs}` (which mirror MoltenVK's object graph).

pub mod command;
pub mod descriptor;
pub mod device;
pub mod instance;
pub mod memory;
pub mod pipeline;
pub mod queue;
