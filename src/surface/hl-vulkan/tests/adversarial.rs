//! Adversarial coverage for the hl-vulkan lowering layer: error paths, boundary conditions, object-model
//! invariants, and the memory-flush/readback bind-offset math — everything a real Vulkan app
//! (vkcube/ANGLE-on-Vulkan) can drive that the happy-path `lowering.rs` suite does not already pin.
//!
//! Every assertion checks REAL recorded IR (`Cmd`/`Enc`), the emitted `WriteBuffer` bytes, the recorded
//! readback requests, or a typed `GpuError` — never merely "did not panic". The bind-offset flush test
//! is a regression for the still-mapped-flush bound-offset bug (a suballocated persistently-mapped buffer
//! flushed the arena from offset 0 instead of the buffer's own footprint).

use hl_vulkan::model::descriptor::{
    vk_descriptor_type, DescriptorTemplateEntry, LayoutBinding,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result::{self, Status};
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::VertexLayout;
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::{BufferId, Cmd, GpuError, RecordingSink};

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

/// The last `CreateBindGroup` descriptor recorded on the sink (the one a bind call just emitted).
fn last_bind_group(s: &RecordingSink) -> hl_gpu::protocol::model::descriptor::BindGroupDesc {
    s.commands()
        .filter_map(|c| match c {
            Cmd::CreateBindGroup(_, desc) => Some(desc.clone()),
            _ => None,
        })
        .last()
        .expect("a CreateBindGroup was recorded")
}

/// Encode a `VkDescriptorBufferInfo` (`{u64 buffer; u64 offset; u64 range}`, 24 bytes LE) into `out`.
fn push_buffer_info(out: &mut Vec<u8>, buffer: u64, offset: u64, range: u64) {
    out.extend_from_slice(&buffer.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&range.to_le_bytes());
}

#[path = "adversarial/api.rs"]
mod api;
#[path = "adversarial/memory.rs"]
mod memory;
#[path = "adversarial/operations.rs"]
mod operations;
#[path = "adversarial/recording.rs"]
mod recording;
