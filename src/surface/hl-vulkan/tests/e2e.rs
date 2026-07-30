//! End-to-end graphics: a clear + a solid-color triangle driven through the REAL hl-vulkan lowering
//! services, an in-process [`InProcessCommandSink`] over the reference [`CpuExecutor`], and the whole
//! host runtime pipeline — then the RASTERIZED render target is read back and its pixels are asserted.
//!
//! This mirrors `hl-cuda`'s `tests/e2e.rs` (which reads back a COMPUTED vecadd), but for the graphics
//! path:
//!
//!   vkCreateImage(RENDER_TARGET) → vkCreateShaderModule ×2 (SPIR-V) → vkCreateGraphicsPipelines
//!   (with a slot-0 vertex layout + one color target) → vkCreateBuffer(VERTEX) + map/write the triangle
//!   → vkBeginCommandBuffer → vkCmdBeginRenderPass(clear) → vkCmdBindPipeline → vkCmdBindVertexBuffers
//!   → vkCmdDraw(3) → vkCmdEndRenderPass → vkQueueSubmit
//!        └─lowers to─▶ protocol Cmds ─submit─▶ InProcessCommandSink
//!             └▶ runtime validate → account → dispatch → CpuExecutor (clears the target, then
//!                rasterizes the triangle from the vertex buffer's pos+color) → read_texture → assert.
//!
//! HONEST LIMITATION (documented, not papered over): the reference [`CpuExecutor`] advertises only the
//! KERNEL shader payload and does NOT execute a SPIR-V/graphics shader — its render path is a fixed-
//! function rasterizer that fetches each vertex's NDC position (bytes 0..8) and straight-alpha color
//! (bytes 8..24) DIRECTLY from the bound slot-0 vertex buffer (see `hl_gpu` `cpu/service/raster.rs`).
//! So this test proves the full lowering seam + clear + triangle GEOMETRY COVERAGE + vertex color, which
//! is everything the CPU oracle can render; it does not (cannot) execute a real fragment shader. The
//! SPIR-V shader modules are created + forwarded verbatim (the seam keystone) and referenced by the
//! pipeline, but the CPU oracle never runs them. To let the permissive lowering create SPIR-V modules
//! against the KERNEL-only oracle, the sink is built with a full (`Capabilities::permissive_fixture`) capability set
//! rather than negotiating the executor's own narrow advertisement.

use hl_vulkan::adapter::spirv;
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result::HL_API_VERSION;
use hl_vulkan::service::{create, present, record, submit};
use hl_vulkan::Instance;

use hl_gpu::protocol::model::descriptor::{VertexAttr, VertexLayout};
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::protocol::model::id::TextureId;
use hl_gpu::{
    Capabilities, CpuExecutor, FakeClock, GlobalLedger, InProcessCommandSink, Limits, Session,
};

/// Pack `[x, y, r, g, b, a]` (6 f32 = 24-byte stride) little-endian — one vertex the CPU rasterizer reads.
fn vertex(x: f32, y: f32, c: [f32; 4]) -> Vec<u8> {
    [x, y, c[0], c[1], c[2], c[3]]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Pack `[x, y, z, r, g, b, a]` (7 f32 = 28-byte stride) little-endian — a vertex carrying an explicit
/// depth `z` (the CPU rasterizer reads z at offset 8 when the stride is ≥ 28; see `raster::read_vertex`).
fn depth_vertex(x: f32, y: f32, z: f32, c: [f32; 4]) -> Vec<u8> {
    [x, y, z, c[0], c[1], c[2], c[3]]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

#[path = "e2e/graphics.rs"]
mod graphics;
#[path = "e2e/memory.rs"]
mod memory;
#[path = "e2e/present.rs"]
mod present_tests;
#[path = "e2e/raster.rs"]
mod raster;
