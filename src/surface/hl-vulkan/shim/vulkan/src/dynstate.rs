//! Extended dynamic state 1/2/3 (`VK_EXT_extended_dynamic_state{,2,3}` + the core-1.3 promotions) — the
//! hand-written `vkCmdSet*` bodies that record fixed-function pipeline state into the command buffer's
//! [`hl_vulkan::model::command::DynamicState`].
//!
//! These are REAL bodies in the same sense the existing `vkCmdSetLineWidth`/`vkCmdSetDepthBias` are: each
//! validates the command buffer is recording and records the value as observable, honest command state
//! through [`record::set_dynamic`]. The software color rasterizer models none of this fixed-function
//! state, so — exactly like the base dynamic state — they carry no encoder op, but the value is retained
//! (never faked, never silently dropped). The three commands that DO lower to real IR are here too:
//! `vkCmdSetViewportWithCount` / `vkCmdSetScissorWithCount` (→ `Enc::SetViewport`/`SetScissor`) and
//! `vkCmdBindVertexBuffers2` (→ `Enc::SetVertexBuffer`), reusing the base recording services.
//!
//! Every `*EXT` alias delegates to its core-named body so the two exports are byte-identical behaviour.

#![allow(clippy::missing_safety_doc)]

mod color;
mod depth_stencil;
mod geometry_view;
mod multisample;
mod rasterization;
mod vertex_tessellation;

mod support;

pub use color::*;
pub use depth_stencil::*;
pub use geometry_view::*;
pub use multisample::*;
pub use rasterization::*;
pub use vertex_tessellation::*;
