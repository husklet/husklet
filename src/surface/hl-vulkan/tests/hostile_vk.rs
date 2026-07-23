//! Adversarial/hostile robustness sweep of the hl-vulkan lowering shim (task #191, the third leg of the
//! executor #188 / GL-shim #189 robustness trilogy).
//!
//! Every test here drives a shim entrypoint with MALFORMED / HOSTILE input — a dangling handle, an
//! out-of-range index/offset, an oversized/zero allocation, an invalid `VkCreateInfo`, an
//! overflow-inducing coordinate, a double-free / use-after-free, or a submit that references a destroyed
//! resource — and asserts the shim:
//!   1. returns the correct typed `GpuError` (→ the honest `VkResult` via
//!      [`Status::from_error`]) OR performs a documented SAFE handling, and
//!   2. NEVER panics / aborts / corrupts its object model (an add-overflow, an unchecked `usize` cast,
//!      or a multi-GiB `Vec` resize would abort the host — those are real bugs, fixed in the shim), and
//!   3. still serves a VALID follow-up call afterward (the shim survives each abuse).
//!
//! Several assertions here are regressions for real panics this sweep found and fixed in the shim
//! (`vkAllocateMemory` host-Vec capacity-overflow on an over-heap size; `write_mapped` /
//! `vkCmdCopyImage` / `vkCmdBlitImage` / `vkCmdBindDescriptorSets` arithmetic overflow; `vkCmdPushConstants`
//! / `vkCmdSet*EXT` multi-GiB `resize`; `vkCmdCopyQueryPoolResults` / `vkGetQueryPoolResults` stride
//! overflow) — see the module report accompanying the change.

use hl_vulkan::model::descriptor::{vk_descriptor_type, LayoutBinding};
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result::{self, Status};
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{Cmd, GpuError, RecordingSink};

fn dev() -> Device {
    let inst = Instance::new(result::HL_API_VERSION);
    inst.create_device()
}
fn sink() -> RecordingSink {
    RecordingSink::with_full_caps()
}
fn buf_ir(d: &Device, h: u64) -> u32 {
    d.buffers.get(&h).unwrap().ir_id
}
/// Allocate → begin a recording primary command buffer.
fn recording_cb(d: &mut Device) -> u64 {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    cb
}

#[path = "hostile_vk/image.rs"]
mod image;
#[path = "hostile_vk/lifecycle.rs"]
mod lifecycle;
#[path = "hostile_vk/memory.rs"]
mod memory;
#[path = "hostile_vk/recording.rs"]
mod recording;
