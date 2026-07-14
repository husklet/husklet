//! One Vulkan operation family per file (OVERVIEW-v2 §2 `service/`).
//!
//! Every IR-emitting function here takes `&mut Device` (the model it mutates) + `&mut dyn CommandSink`
//! (the boundary it submits through), lowers the Vulkan operation into protocol [`hl_gpu::Cmd`]s, and
//! submits them. This is the tested lowering surface: a driver test drives these against a
//! [`hl_gpu::RecordingSink`] and asserts the exact recorded command sequence.
//!
//! Fully lowered this pass: [`create`] (instance/device + buffer/memory/image/sampler/shader/pipeline/
//! descriptor-set/fence creation), [`record`] (`vkCmd*` → `Enc`, incl. descriptor-set → bind group),
//! [`submit`] (`vkQueueSubmit` → `Cmd::Submit`), and [`present`] (`vkCreateSwapchainKHR` /
//! `vkQueuePresentKHR` → `Cmd::CreateSurface`/`Cmd::Present`).

pub mod create;
pub mod present;
pub mod record;
pub mod submit;
