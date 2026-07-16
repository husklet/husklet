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
use hl_vulkan::result::{self, vk_result_from_gpu_error};
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::VertexLayout;
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::{BufferId, Cmd, GpuError, RecordingSink};

fn dev() -> Device {
    let inst = create::create_instance(result::HL_API_VERSION);
    create::create_device(&inst)
}
fn sink() -> RecordingSink {
    RecordingSink::with_full_caps()
}
fn buf_ir(d: &Device, h: u64) -> u32 {
    d.buffers.get(&h).unwrap().ir_id
}

/// Record a command buffer through begin → `f` → end → submit, returning the encoder of the single
/// `Cmd::Submit` in the last batch. Panics if the batch is not exactly one Submit.
fn record_and_submit(d: &mut Device, s: &mut RecordingSink, f: impl FnOnce(&mut Device, u64)) -> Vec<Enc> {
    let cb = record::allocate_command_buffer(d);
    record::begin(d, cb, false).unwrap();
    f(d, cb);
    record::end(d, cb).unwrap();
    submit::queue_submit(d, s, &[cb], None).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => cbuf.encoder.clone(),
        other => panic!("expected a single Submit, got {other:?}"),
    }
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

// =====================================================================================================
// memory: bind-offset flush + readback math (the suballocated-buffer path)
// =====================================================================================================

/// REGRESSION: a persistently-mapped buffer bound at a NON-ZERO offset into its allocation must flush the
/// buffer's own footprint (`data[bound_offset..bound_offset+size]`), NOT the arena from offset 0. Before
/// the fix the still-mapped flush shipped `data[0..size]` — the wrong bytes for any suballocated buffer.
#[test]
fn still_mapped_flush_honors_bind_offset() {
    let mut d = dev();
    let mut s = sink();
    // 32-byte allocation; a 16-byte buffer bound at offset 16 (a second suballocation in one arena).
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = create::allocate_memory(&mut d, 32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    // The app writes the buffer's bytes through the mapped pointer at allocation offset 16.
    let pattern: Vec<u8> = (1..=16u8).collect();
    create::write_mapped(&mut d, mem, 16, &pattern).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();

    // Exactly one WriteBuffer for our buffer, at buffer offset 0, carrying the FOOTPRINT bytes.
    let batch = s.batches.last().unwrap();
    let write = batch
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .expect("a WriteBuffer for the mapped buffer");
    assert_eq!(write.0, 0, "flush targets buffer offset 0");
    assert_eq!(write.1, pattern, "flush carries the buffer footprint, not the arena from offset 0");
}

/// The device→host readback (`vkMapMemory`) also reads from the buffer footprint: a buffer bound at
/// offset 16, mapped over the whole allocation, issues a `read_buffer(ir, 0, 16)` — buffer-relative.
#[test]
fn read_mapped_bind_offset_reads_buffer_relative_range() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = create::allocate_memory(&mut d, 32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    // Map the whole allocation (offset 0, WHOLE_SIZE); only the [16,32) footprint overlaps the buffer.
    create::read_mapped(&mut d, &mut s, mem, 0, u64::MAX).unwrap();
    assert_eq!(s.reads, vec![(BufferId(ir), 0, 16)], "readback is buffer-relative from footprint start");
}

/// A pending (unmapped-before-submit) upload of a suballocated buffer flushes buffer-relative too.
#[test]
fn pending_flush_bind_offset_targets_buffer_relative_offset() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = create::allocate_memory(&mut d, 32).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 16).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    let pattern: Vec<u8> = (100..=115u8).collect();
    create::write_mapped(&mut d, mem, 16, &pattern).unwrap();
    create::unmap_memory(&mut d, mem); // captures pending (0, WHOLE) intersected with footprint

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let write = s
        .batches
        .last()
        .unwrap()
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .expect("pending flush WriteBuffer");
    assert_eq!(write, (0, pattern));
}

/// A pending upload is one-shot: it flushes at the first submit and is retired, so a SECOND submit emits
/// no WriteBuffer for it (the app's staged bytes reach the device exactly once).
#[test]
fn pending_upload_is_cleared_after_one_submit() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 8).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = create::allocate_memory(&mut d, 8).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[9u8; 8]).unwrap();
    create::unmap_memory(&mut d, mem);

    for expect_write in [true, false] {
        let cb = record::allocate_command_buffer(&mut d);
        record::begin(&mut d, cb, false).unwrap();
        record::end(&mut d, cb).unwrap();
        submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
        let has = s.batches.last().unwrap().iter().any(|c| matches!(c, Cmd::WriteBuffer { id, .. } if *id == ir));
        assert_eq!(has, expect_write, "pending upload flushes exactly once");
    }
}

/// `capture_pending_upload` widens an already-pending sub-range so an earlier flush is never lost when a
/// second `vkFlushMappedMemoryRanges` covers a different span.
#[test]
fn flush_ranges_widen_and_cover_both_writes() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 16).unwrap();
    let ir = buf_ir(&d, buf);
    let mem = create::allocate_memory(&mut d, 16).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    let all: Vec<u8> = (1..=16u8).collect();
    create::write_mapped(&mut d, mem, 0, &all).unwrap();
    // Two disjoint sub-range flushes: [0,4) then [12,16). The union must reach [0,16).
    create::capture_pending_upload(&mut d, mem, 0, 4);
    create::capture_pending_upload(&mut d, mem, 12, 4);
    create::unmap_memory(&mut d, mem); // still-mapped? no — unmap keeps pending and widens to whole

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let (off, data) = s
        .batches
        .last()
        .unwrap()
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(off, 0);
    // Covers both ends (byte 0 and byte 15 present with their written values).
    assert_eq!(data[0], 1);
    assert_eq!(data[data.len() - 1], 16);
}

#[test]
fn write_mapped_out_of_range_is_out_of_bounds() {
    let mut d = dev();
    let mem = create::allocate_memory(&mut d, 8).unwrap();
    let err = create::write_mapped(&mut d, mem, 4, &[0u8; 8]).unwrap_err();
    assert!(matches!(err, GpuError::OutOfBounds));
}

#[test]
fn map_and_write_unknown_memory_error() {
    let mut d = dev();
    assert!(matches!(create::map_memory(&mut d, 0xdead), Err(GpuError::Invalid(_))));
    assert!(matches!(create::write_mapped(&mut d, 0xdead, 0, &[0]), Err(GpuError::Invalid(_))));
}

#[test]
fn bind_unknown_memory_or_buffer_errors_and_read_of_unbound_is_noop() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    assert!(matches!(create::bind_buffer_memory(&mut d, buf, 0xdead, 0), Err(GpuError::Invalid(_))));
    let mem = create::allocate_memory(&mut d, 16).unwrap();
    assert!(matches!(create::bind_buffer_memory(&mut d, 0xdead, mem, 0), Err(GpuError::Invalid(_))));
    // read_mapped on host-only staging (no bound buffer) issues no readback and no error.
    create::map_memory(&mut d, mem).unwrap();
    create::read_mapped(&mut d, &mut s, mem, 0, u64::MAX).unwrap();
    assert!(s.reads.is_empty(), "unbound staging has no device source to read back");
}

// =====================================================================================================
// command-buffer lifecycle invariants
// =====================================================================================================

#[test]
fn cmd_outside_recording_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    // Initial (not begun): a vkCmd* must fail.
    assert!(matches!(record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0), Err(GpuError::Invalid(_))));
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    // Executable (ended): a vkCmd* must fail again.
    assert!(matches!(record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0), Err(GpuError::Invalid(_))));
}

#[test]
fn end_without_begin_is_rejected() {
    let mut d = dev();
    let cb = record::allocate_command_buffer(&mut d);
    assert!(matches!(record::end(&mut d, cb), Err(GpuError::Invalid(_))));
}

#[test]
fn begin_unknown_command_buffer_errors() {
    let mut d = dev();
    assert!(matches!(record::begin(&mut d, 0xdead, false), Err(GpuError::Invalid(_))));
    assert!(matches!(record::end(&mut d, 0xdead), Err(GpuError::Invalid(_))));
}

#[test]
fn submit_non_executable_buffer_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let cb = record::allocate_command_buffer(&mut d);
    // Initial state → not executable.
    assert!(matches!(submit::queue_submit(&mut d, &mut s, &[cb], None), Err(GpuError::Invalid(_))));
    // Recording → still not executable.
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(submit::queue_submit(&mut d, &mut s, &[cb], None), Err(GpuError::Invalid(_))));
}

#[test]
fn resubmit_semantics_one_time_vs_reusable() {
    let mut d = dev();
    let mut s = sink();

    // A ONE-TIME-SUBMIT buffer is single-use: after its (synchronous) submit completes it is not
    // resubmittable, so a second submit of the same buffer is rejected.
    let once = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, once, true).unwrap();
    record::end(&mut d, once).unwrap();
    submit::queue_submit(&mut d, &mut s, &[once], None).unwrap();
    assert!(matches!(submit::queue_submit(&mut d, &mut s, &[once], None), Err(GpuError::Invalid(_))));

    // A REUSABLE buffer (no ONE_TIME_SUBMIT) records once and re-submits every frame — the vkcube
    // per-image draw pattern. The synchronous executor completes each submit, returning it to Executable,
    // so repeated submits all succeed.
    let reuse = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, reuse, false).unwrap();
    record::end(&mut d, reuse).unwrap();
    for _ in 0..5 {
        submit::queue_submit(&mut d, &mut s, &[reuse], None).unwrap();
    }
}

#[test]
fn submit_unknown_command_buffer_errors() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(submit::queue_submit(&mut d, &mut s, &[0xdead], None), Err(GpuError::Invalid(_))));
}

#[test]
fn begin_resets_prior_recording() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap();
    // A fresh begin must clear the earlier SetVertexBuffer.
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert!(cbuf.encoder.is_empty(), "begin cleared the prior recording"),
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// buffers / images: destroy, use-after-free, double-destroy
// =====================================================================================================

#[test]
fn use_after_destroy_buffer_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0), Err(GpuError::Invalid(_))));
}

#[test]
fn double_destroy_buffer_emits_destroy_once() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    let n = s.batches.len();
    create::destroy_buffer(&mut d, &mut s, buf).unwrap(); // no-op
    assert_eq!(s.batches.len(), n, "second destroy emits nothing");
    assert!(s.commands().any(|c| matches!(c, Cmd::DestroyBuffer(i) if *i == ir)));
}

// =====================================================================================================
// shader / spirv adapter boundaries
// =====================================================================================================

#[test]
fn shader_module_rejects_malformed_spirv() {
    let mut d = dev();
    let mut s = sink();
    // Not a multiple of 4.
    assert!(create::create_shader_module(&mut d, &mut s, &[1, 2, 3]).is_err());
    // Multiple of 4 but shorter than the 5-word header.
    assert!(create::create_shader_module(&mut d, &mut s, &[0u8; 8]).is_err());
    // Full header length but wrong magic.
    let bad_magic: Vec<u8> = [1u32, 0, 0, 0, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    assert!(matches!(create::create_shader_module(&mut d, &mut s, &bad_magic), Err(GpuError::Invalid(_))));
}

// =====================================================================================================
// pipelines: missing entry points, unknown modules
// =====================================================================================================

#[test]
fn compute_pipeline_unknown_module_and_missing_entry() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(create::create_compute_pipeline(&mut d, &mut s, 0xdead, "main"), Err(GpuError::Invalid(_))));
    let sh = create::create_shader_module_words(&mut d, &mut s, hl_vulkan::adapter::spirv::sample_compute_spirv("main")).unwrap();
    assert!(matches!(create::create_compute_pipeline(&mut d, &mut s, sh, "nope"), Err(GpuError::Invalid(_))));
}

#[test]
fn graphics_pipeline_rejects_missing_fragment_entry() {
    let mut d = dev();
    let mut s = sink();
    let vs = create::create_shader_module_words(&mut d, &mut s, hl_vulkan::adapter::spirv::sample_compute_spirv("vs")).unwrap();
    let fs = create::create_shader_module_words(&mut d, &mut s, hl_vulkan::adapter::spirv::sample_compute_spirv("fs")).unwrap();
    // Bad fragment entry → the whole pipeline fails (no id-zero default).
    let r = create::create_graphics_pipeline(&mut d, &mut s, (vs, "vs"), Some((fs, "bad")), vec![], vec![TextureFormat::Rgba8Unorm], None, None, 1, Topology::TriangleList, 0, 0, 0xf);
    assert!(matches!(r, Err(GpuError::Invalid(_))));
}

#[test]
fn graphics_pipeline_with_no_color_targets_is_valid() {
    let mut d = dev();
    let mut s = sink();
    let vs = create::create_shader_module_words(&mut d, &mut s, hl_vulkan::adapter::spirv::sample_compute_spirv("vs")).unwrap();
    // Depth-only / no-color pipeline: an empty color-format slice is valid.
    let pipe = create::create_graphics_pipeline(&mut d, &mut s, (vs, "vs"), None, Vec::<VertexLayout>::new(), vec![], None, None, 1, Topology::TriangleList, 0, 0, 0xf).unwrap();
    match s.commands().find(|c| matches!(c, Cmd::CreateRenderPipeline(..))).unwrap() {
        Cmd::CreateRenderPipeline(_, desc) => {
            assert!(desc.color_targets.is_empty());
            assert!(desc.fragment.is_none());
        }
        _ => unreachable!(),
    }
    assert!(d.pipelines.contains_key(&pipe));
}

// =====================================================================================================
// descriptors: pools, dynamic offsets, multiple sets
// =====================================================================================================

#[test]
fn descriptor_pool_exhaustion_and_unknown_pool() {
    let mut d = dev();
    let layout = create::create_descriptor_set_layout(&mut d, vec![]);
    assert!(matches!(create::allocate_descriptor_set(&mut d, 0xdead, layout, 0), Err(GpuError::Invalid(_))));
    let pool = create::create_descriptor_pool(&mut d, 1);
    create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // The pool's single set is consumed → the second allocation is a resource-limit error.
    let err = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap_err();
    assert!(matches!(err, GpuError::ResourceLimit(_)));
    assert_eq!(vk_result_from_gpu_error(&err), result::VK_ERROR_OUT_OF_DEVICE_MEMORY);
}

#[test]
fn update_descriptor_unknown_set_errors() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    assert!(matches!(create::update_descriptor_buffer(&mut d, 0xdead, 0, buf, 0, 16), Err(GpuError::Invalid(_))));
    assert!(matches!(create::update_descriptor_image(&mut d, 0xdead, 0, None, None), Err(GpuError::Invalid(_))));
}

#[test]
fn dynamic_offsets_apply_to_dynamic_bindings_only() {
    let mut d = dev();
    let mut s = sink();
    let b0 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let b2 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let (ir0, ir2) = (buf_ir(&d, b0), buf_ir(&d, b2));
    // binding 0 = static storage; binding 2 = dynamic uniform (consumes one pDynamicOffset).
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![
            LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 },
            LayoutBinding { binding: 2, descriptor_type: vk_descriptor_type::UNIFORM_BUFFER_DYNAMIC, descriptor_count: 1, stage_flags: 0 },
        ],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, 0, b0, 0, 256).unwrap();
    create::update_descriptor_buffer(&mut d, set, 2, b2, 8, 64).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[100]).unwrap();
    let bg = last_bind_group(&s);
    use hl_gpu::protocol::model::descriptor::BindResource;
    for e in &bg.entries {
        let BindResource::Buffer { id, offset, .. } = &e.resource else {
            panic!("expected a buffer resource, got {:?}", e.resource);
        };
        match e.binding {
            0 => { assert_eq!(*id, ir0); assert_eq!(*offset, 0); }
            2 => { assert_eq!(*id, ir2); assert_eq!(*offset, 8 + 100); }
            b => panic!("unexpected binding {b}"),
        }
    }
    assert_eq!(bg.entries.len(), 2);
}

#[test]
fn multiple_sets_get_distinct_set_indices_from_first_set() {
    let mut d = dev();
    let mut s = sink();
    let ba = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let bb = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 }],
    );
    let pool = create::create_descriptor_pool(&mut d, 2);
    let sa = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let sb = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, sa, 0, ba, 0, 64).unwrap();
    create::update_descriptor_buffer(&mut d, sb, 0, bb, 0, 64).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // first_set = 1 → the two sets land at set indices 1 and 2.
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 1, &[sa, sb], &[]).unwrap();
    let sets: Vec<u32> = s.commands().filter_map(|c| match c {
        Cmd::CreateBindGroup(_, desc) => Some(desc.set),
        _ => None,
    }).collect();
    assert_eq!(sets, vec![1, 2]);
}

#[test]
fn separate_image_and_sampler_writes_compose_on_one_binding() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(&mut d, &mut s, 4, 4, vk_format::R8G8B8A8_UNORM, vk_image_usage::SAMPLED, 1).unwrap();
    let samp = create::create_sampler(&mut d, &mut s, 1, 1, 1, [0, 0, 0]);
    let img_ir = d.images.get(&img).unwrap().ir_id;
    let samp_ir = d.samplers.get(&samp).unwrap().ir_id;
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding { binding: 3, descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER, descriptor_count: 1, stage_flags: 0 }],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // Two separate writes to the SAME binding: image first, sampler later — must compose (both survive).
    create::update_descriptor_image(&mut d, set, 3, Some(img), None).unwrap();
    create::update_descriptor_image(&mut d, set, 3, None, Some(samp)).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[]).unwrap();
    use hl_gpu::protocol::model::descriptor::BindResource;
    let bg = last_bind_group(&s);
    // Combined descriptor at binding 3: the image stays at binding 3, the sampler splits to binding 3 + 16
    // (the executor's `spirv_split` scheme — a combined image-sampler occupies two distinct bind-group slots).
    assert!(bg.entries.iter().any(|e| e.binding == 3 && matches!(e.resource, BindResource::Texture { id } if id == img_ir)));
    assert!(bg.entries.iter().any(|e| e.binding == 19 && matches!(e.resource, BindResource::Sampler { id } if id == samp_ir)));
}

// =====================================================================================================
// transfer / copy validation
// =====================================================================================================

#[test]
fn copy_buffer_to_image_usage_and_bounds_errors() {
    let mut d = dev();
    let mut s = sink();
    // src lacks TRANSFER_SRC.
    let bad_src = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 4096).unwrap();
    let img = create::create_image(&mut d, &mut s, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST, 1).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_copy_buffer_to_image(&mut d, cb, bad_src, img, 0, 0, 0, 8, 8), Err(GpuError::Invalid(_))));
    // A good src but an oversized region (width > image width) is out of bounds.
    let src = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_SRC, 4096).unwrap();
    assert!(matches!(record::cmd_copy_buffer_to_image(&mut d, cb, src, img, 0, 0, 0, 16, 8), Err(GpuError::OutOfBounds)));
}

#[test]
fn copy_image_format_mismatch_and_self_overlap_rejected() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(&mut d, &mut s, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST, 1).unwrap();
    let b = create::create_image(&mut d, &mut s, 8, 8, vk_format::B8G8R8A8_UNORM, vk_image_usage::TRANSFER_DST, 1).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Format mismatch.
    assert!(matches!(record::cmd_copy_image(&mut d, cb, a, b, (0, 0), (0, 0), (4, 4)), Err(GpuError::Invalid(_))));
    // Overlapping same-image self-copy.
    assert!(matches!(record::cmd_copy_image(&mut d, cb, a, a, (0, 0), (2, 2), (4, 4)), Err(GpuError::Invalid(_))));
    // A non-overlapping same-image copy is allowed.
    assert!(record::cmd_copy_image(&mut d, cb, a, a, (0, 0), (4, 0), (4, 4)).is_ok());
}

#[test]
fn blit_same_image_rejected_and_zero_extent_rejected() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(&mut d, &mut s, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST, 1).unwrap();
    let b = create::create_image(&mut d, &mut s, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST, 1).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_blit_image(&mut d, cb, a, a, (0, 0), (4, 4), (0, 0), (4, 4), true), Err(GpuError::Invalid(_))));
    assert!(matches!(record::cmd_blit_image(&mut d, cb, a, b, (0, 0), (0, 4), (0, 0), (4, 4), false), Err(GpuError::OutOfBounds)));
}

#[test]
fn fill_buffer_alignment_usage_and_whole_size() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let ir = buf_ir(&d, buf);
    let no_dst = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 64).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Misaligned dstOffset.
    assert!(matches!(record::cmd_fill_buffer(&mut d, cb, buf, 3, 4, 0), Err(GpuError::Invalid(_))));
    // Missing COPY_DST usage.
    assert!(matches!(record::cmd_fill_buffer(&mut d, cb, no_dst, 0, 4, 0), Err(GpuError::Invalid(_))));
    // Out-of-bounds size.
    assert!(matches!(record::cmd_fill_buffer(&mut d, cb, buf, 0, 128, 0), Err(GpuError::OutOfBounds)));
    // VK_WHOLE_SIZE fills to the end and flushes as a WriteBuffer of the whole buffer (64 bytes = 16 words).
    record::cmd_fill_buffer(&mut d, cb, buf, 0, u64::MAX, 0xAA).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let (off, data) = s.batches.last().unwrap().iter().find_map(|c| match c {
        Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
        _ => None,
    }).unwrap();
    assert_eq!(off, 0);
    assert_eq!(data.len(), 64);
    // vkCmdFillBuffer replicates the 32-bit `data` word (0x000000AA) across the range, little-endian.
    assert!(data.chunks_exact(4).all(|w| w == [0xAA, 0, 0, 0]));
}

#[test]
fn update_buffer_size_limits() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Empty data.
    assert!(matches!(record::cmd_update_buffer(&mut d, cb, buf, 0, &[]), Err(GpuError::Invalid(_))));
    // Not a multiple of 4.
    assert!(matches!(record::cmd_update_buffer(&mut d, cb, buf, 0, &[1, 2, 3]), Err(GpuError::Invalid(_))));
    // Out of bounds.
    assert!(matches!(record::cmd_update_buffer(&mut d, cb, buf, 60, &[0u8; 8]), Err(GpuError::OutOfBounds)));
}

#[test]
fn clear_color_image_requires_copy_dst() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(&mut d, &mut s, 4, 4, vk_format::R8G8B8A8_UNORM, vk_image_usage::SAMPLED, 1).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_clear_color_image(&mut d, cb, img, [1.0; 4]), Err(GpuError::Invalid(_))));
}

#[test]
fn clear_attachments_outside_render_pass_errors() {
    let mut d = dev();
    let mut s = sink();
    let _ = &mut s;
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Non-empty rect, no active render pass → error.
    assert!(matches!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [1.0; 4]), Err(GpuError::Invalid(_))));
    // A zero-area rect is a spec-valid no-op even outside a pass.
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 0, 0, [1.0; 4]).is_ok());
}

// =====================================================================================================
// indirect draws / dispatch validation
// =====================================================================================================

#[test]
fn indirect_validation_missing_usage_and_out_of_range() {
    let mut d = dev();
    let mut s = sink();
    // Missing INDIRECT usage.
    let plain = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_draw_indirect(&mut d, cb, plain, 0, 1, 16), Err(GpuError::Invalid(_))));
    // Proper INDIRECT buffer but the argument span runs past the end.
    let ind = create::create_buffer(&mut d, &mut s, vk_buffer_usage::INDIRECT_BUFFER, 16).unwrap();
    assert!(matches!(record::cmd_draw_indirect(&mut d, cb, ind, 0, 2, 16), Err(GpuError::OutOfBounds)));
    // A zero-count indirect draw is a valid no-op that records nothing.
    assert!(record::cmd_draw_indirect(&mut d, cb, ind, 0, 0, 16).is_ok());
    // Dispatch-indirect needs 12 bytes; a 8-byte buffer is out of range.
    let small = create::create_buffer(&mut d, &mut s, vk_buffer_usage::INDIRECT_BUFFER, 8).unwrap();
    assert!(matches!(record::cmd_dispatch_indirect(&mut d, cb, small, 0), Err(GpuError::OutOfBounds)));
}

// =====================================================================================================
// push constants
// =====================================================================================================

#[test]
fn push_constants_alignment_rules() {
    let mut d = dev();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_push_constants(&mut d, cb, 2, &[0u8; 4]), Err(GpuError::Invalid(_)))); // offset misaligned
    assert!(matches!(record::cmd_push_constants(&mut d, cb, 0, &[0u8; 3]), Err(GpuError::Invalid(_)))); // size misaligned
    assert!(matches!(record::cmd_push_constants(&mut d, cb, 0, &[]), Err(GpuError::Invalid(_)))); // empty
    assert!(record::cmd_push_constants(&mut d, cb, 4, &[1, 2, 3, 4]).is_ok());
    // Recorded at the offset (grown on demand): bytes [0..4) stay zero, [4..8) hold the write.
    assert_eq!(d.command_buffers.get(&cb).unwrap().push_constants, vec![0, 0, 0, 0, 1, 2, 3, 4]);
}

// =====================================================================================================
// pipeline barriers
// =====================================================================================================

#[test]
fn pipeline_barrier_records_known_and_skips_unknown() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(&mut d, &mut s, 4, 4, vk_format::R8G8B8A8_UNORM, vk_image_usage::COLOR_ATTACHMENT, 1).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Known image: layout recorded; unknown image: skipped (no panic, no entry).
    record::cmd_pipeline_barrier(&mut d, cb, &[(img, 0, 7), (0xdead, 0, 7)]).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    assert_eq!(d.image_layouts.get(&img), Some(&7));
    assert!(!d.image_layouts.contains_key(&0xdead));
    // No IR is emitted by the barrier — the Submit encoder is empty.
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert!(cbuf.encoder.is_empty()),
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// events / semaphores
// =====================================================================================================

#[test]
fn event_host_lifecycle_and_unknown_errors() {
    let mut d = dev();
    let e = sync::create_event(&mut d);
    assert!(!sync::event_status(&d, e).unwrap());
    sync::set_event(&mut d, e, true).unwrap();
    assert!(sync::event_status(&d, e).unwrap());
    sync::set_event(&mut d, e, false).unwrap();
    assert!(!sync::event_status(&d, e).unwrap());
    assert!(matches!(sync::set_event(&mut d, 0xdead, true), Err(GpuError::Invalid(_))));
    assert!(matches!(sync::event_status(&d, 0xdead), Err(GpuError::Invalid(_))));
    sync::destroy_event(&mut d, e);
    assert!(matches!(sync::event_status(&d, e), Err(GpuError::Invalid(_))));
}

#[test]
fn cmd_set_event_unknown_is_rejected() {
    let mut d = dev();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    assert!(matches!(record::cmd_set_event(&mut d, cb, 0xdead, true), Err(GpuError::Invalid(_))));
    assert!(matches!(record::cmd_wait_events(&mut d, cb, &[0xdead]), Err(GpuError::Invalid(_))));
}

#[test]
fn timeline_semaphore_monotonic_and_binary_rejected() {
    let mut d = dev();
    let t = sync::create_semaphore(&mut d, true, 5);
    assert_eq!(sync::semaphore_counter(&d, t).unwrap(), 5);
    sync::signal_semaphore(&mut d, t, 10).unwrap();
    assert_eq!(sync::semaphore_counter(&d, t).unwrap(), 10);
    // A signal to a lower value never regresses the counter.
    sync::signal_semaphore(&mut d, t, 3).unwrap();
    assert_eq!(sync::semaphore_counter(&d, t).unwrap(), 10);
    // A binary semaphore has no counter / cannot be host-signalled by value.
    let b = sync::create_semaphore(&mut d, false, 0);
    assert!(matches!(sync::signal_semaphore(&mut d, b, 1), Err(GpuError::Invalid(_))));
    assert!(matches!(sync::semaphore_counter(&d, b), Err(GpuError::Invalid(_))));
}

#[test]
fn wait_semaphores_any_all_and_empty() {
    let mut d = dev();
    let a = sync::create_semaphore(&mut d, true, 5);
    let b = sync::create_semaphore(&mut d, true, 0);
    // wait-all: a reached (>=5), b not (>=1) → false. wait-any → true.
    assert!(!sync::wait_semaphores(&d, &[a, b], &[5, 1], false));
    assert!(sync::wait_semaphores(&d, &[a, b], &[5, 1], true));
    // An empty wait is trivially satisfied.
    assert!(sync::wait_semaphores(&d, &[], &[], false));
    // An unknown semaphore counts as unreached.
    assert!(!sync::wait_semaphores(&d, &[0xdead], &[0], false));
}

// =====================================================================================================
// query pools
// =====================================================================================================

#[test]
fn query_pool_zero_count_rejected_and_span_bounds() {
    let mut d = dev();
    assert!(matches!(sync::create_query_pool(&mut d, 2, 0), Err(GpuError::Invalid(_))));
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    // Out-of-range reset span.
    assert!(matches!(record::cmd_reset_query_pool(&mut d, cb, pool, 2, 4), Err(GpuError::Invalid(_))));
    // Out-of-range write index.
    assert!(matches!(record::cmd_write_timestamp(&mut d, cb, pool, 4), Err(GpuError::Invalid(_))));
    // Unknown pool.
    assert!(matches!(record::cmd_write_timestamp(&mut d, cb, 0xdead, 0), Err(GpuError::Invalid(_))));
}

#[test]
fn get_query_pool_results_availability_wait_partial() {
    let mut d = dev();
    let mut s = sink();
    let pool = sync::create_query_pool(&mut d, 2, 1).unwrap();
    // Unknown pool + out-of-range are typed errors.
    assert!(matches!(sync::get_query_pool_results(&d, 0xdead, 0, 1, &mut [0u8; 8], 8, true, false, false, false), Err(GpuError::Invalid(_))));
    assert!(matches!(sync::get_query_pool_results(&d, pool, 0, 2, &mut [0u8; 8], 8, true, false, false, false), Err(GpuError::Invalid(_))));
    // Unavailable slot, no WAIT/PARTIAL → NOT_READY (Ok(false)).
    let mut out = [0u8; 4];
    assert!(!sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false).unwrap());
    // WAIT forces a write even while unavailable → Ok(true) in the synchronous model.
    assert!(sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, true, false, false).unwrap());
    // Availability query: the availability word reports 0 (unavailable) for an untouched slot.
    let mut wide = [0u8; 8];
    sync::get_query_pool_results(&d, pool, 0, 1, &mut wide, 8, false, false, true, false).unwrap();
    assert_eq!(u32::from_le_bytes([wide[4], wide[5], wide[6], wide[7]]), 0);
    // After a device write-timestamp submit, the slot is available with a monotonic serial.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let mut out = [0u8; 4];
    assert!(sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false).unwrap());
    assert_eq!(u32::from_le_bytes(out), 1);
}

// =====================================================================================================
// fences
// =====================================================================================================

#[test]
fn fence_status_reset_and_fence_only_submit_signals() {
    let mut d = dev();
    let mut s = sink();
    let signaled = create::create_fence(&mut d, &mut s, true).unwrap();
    assert!(submit::fence_status(&d, signaled).unwrap());
    submit::reset_fence(&mut d, signaled).unwrap();
    assert!(!submit::fence_status(&d, signaled).unwrap());
    assert!(matches!(submit::fence_status(&d, 0xdead), Err(GpuError::Invalid(_))));

    // A fence-only submit (no command buffers) still emits one empty Submit that signals the fence.
    let fence = create::create_fence(&mut d, &mut s, false).unwrap();
    let fence_ir = d.fences.get(&fence).unwrap().ir_id;
    submit::queue_submit(&mut d, &mut s, &[], Some(fence)).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert!(cbuf.encoder.is_empty());
            assert_eq!(cbuf.signal.map(|(ir, _)| ir), Some(fence_ir));
        }
        other => panic!("expected a fence-only Submit, got {other:?}"),
    }
    // Waiting the fence blocks on the sink at the signalled value and marks it signaled.
    submit::wait_for_fence(&mut d, &mut s, fence).unwrap();
    assert!(submit::fence_status(&d, fence).unwrap());
    assert!(!s.waits.is_empty());
}

#[test]
fn submit_unknown_fence_errors_before_emitting() {
    let mut d = dev();
    let mut s = sink();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    assert!(matches!(submit::queue_submit(&mut d, &mut s, &[cb], Some(0xdead)), Err(GpuError::Invalid(_))));
    assert!(s.batches.is_empty(), "a bad fence fails before any Cmd is submitted");
}

// =====================================================================================================
// present / WSI
// =====================================================================================================

#[test]
fn present_unknown_swapchain_and_out_of_range_index() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(present::create_swapchain(&mut d, &mut s, 0xdead, 2), Err(GpuError::Invalid(_))));
    assert!(matches!(present::acquire_next_image(&mut d, 0xdead), Err(GpuError::Invalid(_))));
    assert!(matches!(present::queue_present(&mut d, &mut s, 0xdead, 0), Err(GpuError::Invalid(_))));

    let surf = present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, 7).unwrap();
    let sc = present::create_swapchain(&mut d, &mut s, surf, 2).unwrap();
    assert_eq!(present::acquire_next_image(&mut d, sc).unwrap(), 0);
    // An image index past the swapchain image count is rejected.
    assert!(matches!(present::queue_present(&mut d, &mut s, sc, 99), Err(GpuError::Invalid(_))));
    // A valid present emits Cmd::Present naming the surface's ir + the presented image's REAL texture id.
    present::queue_present(&mut d, &mut s, sc, 0).unwrap();
    let surf_ir = d.surfaces.get(&surf).unwrap().ir_id;
    let img0_ir = d.swapchains.get(&sc).unwrap().images[0].ir_texture_id;
    assert!(s.commands().any(|c| matches!(c, Cmd::Present { surface, texture } if *surface == surf_ir && *texture == img0_ir)));
}

#[test]
fn surface_queries_report_modeled_values() {
    assert!(present::surface_supports_present(0));
    assert!(!present::surface_supports_present(1));
    let caps = present::surface_capabilities();
    assert_eq!(caps.min_image_count, 2);
    assert_eq!(caps.max_image_count, 3);
    assert_eq!(present::surface_present_modes(), vec![2]); // FIFO
    assert_eq!(present::surface_formats().len(), 4);
}

// =====================================================================================================
// pipeline cache
// =====================================================================================================

#[test]
fn pipeline_cache_roundtrip_merge_and_unknown() {
    let mut d = dev();
    let header = create::pipeline_cache_header(&d);
    assert_eq!(header.len(), 32);
    assert_eq!(u32::from_le_bytes([header[0], header[1], header[2], header[3]]), 32); // length field
    assert_eq!(u32::from_le_bytes([header[4], header[5], header[6], header[7]]), 1); // version ONE

    // A short initial blob falls back to a fresh valid header.
    let c = create::create_pipeline_cache(&mut d, &[1, 2, 3]);
    assert_eq!(create::get_pipeline_cache_data(&d, c).unwrap().len(), 32);
    // A >=32-byte initial blob is retained verbatim.
    let mut blob = header.clone();
    blob.extend_from_slice(&[7u8; 8]);
    let c2 = create::create_pipeline_cache(&mut d, &blob);
    assert_eq!(create::get_pipeline_cache_data(&d, c2).unwrap(), blob);
    // Merge validates handles.
    assert!(create::merge_pipeline_caches(&d, c, &[c2]).is_ok());
    assert!(matches!(create::merge_pipeline_caches(&d, 0xdead, &[c2]), Err(GpuError::Invalid(_))));
    assert!(matches!(create::merge_pipeline_caches(&d, c, &[0xdead]), Err(GpuError::Invalid(_))));
    assert!(matches!(create::get_pipeline_cache_data(&d, 0xdead), Err(GpuError::Invalid(_))));
    create::destroy_pipeline_cache(&mut d, c);
    assert!(matches!(create::get_pipeline_cache_data(&d, c), Err(GpuError::Invalid(_))));
}

// =====================================================================================================
// descriptor update templates
// =====================================================================================================

#[test]
fn descriptor_template_array_stride_and_short_blob() {
    let mut d = dev();
    let mut s = sink();
    let b0 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let b1 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let (ir0, ir1) = (buf_ir(&d, b0), buf_ir(&d, b1));
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 2, stage_flags: 0 }],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();

    // One entry: 2 array elements of a buffer descriptor at offset 0, stride 24 (sizeof VkDescriptorBufferInfo).
    let entry = DescriptorTemplateEntry {
        dst_binding: 0,
        dst_array_element: 0,
        descriptor_count: 2,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        offset: 0,
        stride: 24,
    };
    let tmpl = create::create_descriptor_update_template(&mut d, VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET, vec![entry]).unwrap();
    // The exact byte count the shim must present: offset + (count-1)*stride + 24 = 48.
    assert_eq!(create::descriptor_template_data_len(&d, tmpl), Some(48));

    let mut data = Vec::new();
    push_buffer_info(&mut data, b0, 0, 128);
    push_buffer_info(&mut data, b1, 8, 64);
    create::update_descriptor_set_with_template(&mut d, set, tmpl, &data).unwrap();
    // Both array elements fold onto binding 0 (the model keys by binding); the LAST write wins → b1.
    let rec = d.descriptor_sets.get(&set).unwrap();
    assert_eq!(rec.buffers.get(&0), Some(&(b1, 8, 64)));
    let _ = (ir0, ir1);

    // A short blob (one struct missing its tail) is a truthful OutOfBounds, never a junk read.
    let short = &data[..data.len() - 1];
    assert!(matches!(create::update_descriptor_set_with_template(&mut d, set, tmpl, short), Err(GpuError::OutOfBounds)));
    // Unknown template / set.
    assert!(matches!(create::update_descriptor_set_with_template(&mut d, set, 0xdead, &data), Err(GpuError::Invalid(_))));
    assert!(matches!(create::update_descriptor_set_with_template(&mut d, 0xdead, tmpl, &data), Err(GpuError::Invalid(_))));
}

#[test]
fn descriptor_template_wrong_type_is_unsupported() {
    let mut d = dev();
    // Only DESCRIPTOR_SET (0) templates are modeled; PUSH_DESCRIPTORS (1) is a truthful FEATURE_NOT_PRESENT.
    let err = create::create_descriptor_update_template(&mut d, 1, vec![]).unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)));
    assert_eq!(vk_result_from_gpu_error(&err), result::VK_ERROR_FEATURE_NOT_PRESENT);
}

// =====================================================================================================
// secondary command buffers
// =====================================================================================================

#[test]
fn execute_commands_requires_recording_primary_and_executable_secondaries() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    // A recorded, ended (Executable) secondary.
    let sec = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, sec, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, sec, 0, buf, 0).unwrap();
    record::end(&mut d, sec).unwrap();

    // A non-recording primary is rejected.
    let prim0 = record::allocate_command_buffer(&mut d);
    assert!(matches!(record::cmd_execute_commands(&mut d, prim0, &[sec]), Err(GpuError::Invalid(_))));

    // A recording primary + a NON-executable secondary is rejected, and splices nothing.
    let prim = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, prim, false).unwrap();
    let not_ready = record::allocate_command_buffer(&mut d);
    assert!(matches!(record::cmd_execute_commands(&mut d, prim, &[not_ready]), Err(GpuError::Invalid(_))));
    assert!(d.command_buffers.get(&prim).unwrap().enc.is_empty());

    // The valid splice replays the secondary's ops into the primary.
    record::cmd_execute_commands(&mut d, prim, &[sec]).unwrap();
    record::end(&mut d, prim).unwrap();
    submit::queue_submit(&mut d, &mut s, &[prim], None).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert_eq!(cbuf.encoder, vec![Enc::SetVertexBuffer { slot: 0, buffer: ir, offset: 0 }]),
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// image subresource layout
// =====================================================================================================

#[test]
fn image_subresource_layout_and_unknown() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(&mut d, &mut s, 10, 6, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST, 1).unwrap();
    let layout = create::image_subresource_layout(&d, img).unwrap();
    assert_eq!(layout.offset, 0);
    assert_eq!(layout.row_pitch, 40); // width*4
    assert_eq!(layout.size, 240); // row_pitch*height
    assert!(matches!(create::image_subresource_layout(&d, 0xdead), Err(GpuError::Invalid(_))));
}

// =====================================================================================================
// result mapping
// =====================================================================================================

#[test]
fn gpu_error_maps_to_expected_vk_results() {
    assert_eq!(vk_result_from_gpu_error(&GpuError::Invalid("x")), result::VK_ERROR_INITIALIZATION_FAILED);
    assert_eq!(vk_result_from_gpu_error(&GpuError::OutOfBounds), result::VK_ERROR_MEMORY_MAP_FAILED);
    assert_eq!(vk_result_from_gpu_error(&GpuError::Unsupported("x")), result::VK_ERROR_FEATURE_NOT_PRESENT);
    assert_eq!(vk_result_from_gpu_error(&GpuError::ResourceLimit("x")), result::VK_ERROR_OUT_OF_DEVICE_MEMORY);
}

// =====================================================================================================
// instance version handling
// =====================================================================================================

#[test]
fn instance_records_requested_api_version() {
    let older = result::make_api_version(0, 1, 0, 0);
    let inst = Instance::new(older);
    assert_eq!(inst.app_api_version, older);
    // The physical device always advertises the ICD's own (1.4) version regardless of the app request.
    assert_eq!(inst.physical_device.api_version, result::HL_API_VERSION);
}
