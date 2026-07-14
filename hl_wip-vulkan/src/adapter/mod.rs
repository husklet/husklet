//! External, tech-named mechanisms the Vulkan driver drives (OVERVIEW-v2 §2 `adapter/`).
//!
//! * [`spirv`] — the SPIR-V front-end. Unlike hl-cuda's `adapter::ptx` (which *translates* PTX text to
//!   neutral kernel-IR), Vulkan's shader ABI IS SPIR-V and the hl-GPU IR shader ABI is ALSO SPIR-V, so
//!   this adapter is a **passthrough**: it validates the SPIR-V header, extracts the `OpEntryPoint`
//!   names (for pipeline entry resolution), and forwards the words verbatim into
//!   [`hl_gpu::Cmd::CreateShader`] with NO translation. This is the whole reason Vulkan is a natural
//!   fit for the IR.

pub mod spirv;
pub mod wayland_app;
