//! Lowering tests: drive each Vulkan service against a `hl_gpu::RecordingSink` and assert the exact
//! protocol `Cmd`/`Enc` sequence the operation lowers to (plus the SPIR-V passthrough adapter).
//!
//! This is the acceptance gate for the Vulkan→IR lowering layer: no loader, no socket, no GPU — just
//! the recorded command stream, which is wire-identical to what the shipping ICD emits.

use hl_vulkan::adapter::spirv;
use hl_vulkan::model::descriptor::{
    vk_descriptor_type, DescriptorTemplateEntry, LayoutBinding,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage, SubresourceRange};
use hl_vulkan::result;
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::SubresourceLayers;
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindResource, Extent3d, FrameSerial, Origin3d, PipelineBinding, SurfaceToken,
    TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, Filter, IndexFormat, LoadOp, TextureFormat, Topology,
};
use hl_gpu::{Cmd, FenceId, GpuError, RecordingSink, ShaderPayloadKind};

/// A slot-0 vertex layout carrying interleaved position (offset 0) + color (offset 8), stride 24 — the
/// layout the host rasterizer fetches `pos`/`color` from.
fn pos_color_layout() -> VertexLayout {
    VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: 0,
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: 0,
                offset: 8,
            },
        ],
    }
}

fn dev() -> Device {
    let inst = Instance::new(result::HL_API_VERSION);
    inst.create_device()
}

fn buf_ir(d: &Device, h: u64) -> u32 {
    d.buffers.get(&h).unwrap().ir_id
}
fn img_ir(d: &Device, h: u64) -> u32 {
    d.images.get(&h).unwrap().ir_id
}

/// Open a command buffer for recording, for tests that assert on a REFUSAL rather than a stream.
fn begin(d: &mut Device, _sink: &mut RecordingSink) -> u64 {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    cb
}

/// Record `record_fn` into a fresh command buffer and return the single submitted encoder stream.
/// A command buffer left in the RECORDING state, for tests that assert on a record-time refusal rather
/// than on the encoder a successful recording produces.
fn recording_cb(d: &mut Device) -> u64 {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    cb
}

fn record_and_submit(
    d: &mut Device,
    sink: &mut RecordingSink,
    record_fn: impl FnOnce(&mut Device, u64),
) -> Vec<Enc> {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record_fn(d, cb);
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(d, sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => cbuf.encoder.clone(),
        other => panic!("expected a single Submit, got {other:?}"),
    }
}

#[path = "lowering/clear.rs"]
mod clear;
#[path = "lowering/compute_descriptors.rs"]
mod compute_descriptors;
#[path = "lowering/copy.rs"]
mod copy;
#[path = "lowering/descriptor_template.rs"]
mod descriptor_template;
#[path = "lowering/dynamic.rs"]
mod dynamic;
#[path = "lowering/instance_resources.rs"]
mod instance_resources;
#[path = "lowering/memory_submit.rs"]
mod memory_submit;
#[path = "lowering/pipelines.rs"]
mod pipelines;
#[path = "lowering/present.rs"]
mod presentation;
#[path = "lowering/render.rs"]
mod render;
#[path = "lowering/result_dynamic.rs"]
mod result_dynamic;
#[path = "lowering/secondary_surface.rs"]
mod secondary_surface;
#[path = "lowering/sync_query.rs"]
mod sync_query;
