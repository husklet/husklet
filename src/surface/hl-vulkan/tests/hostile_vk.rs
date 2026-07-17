//! Adversarial/hostile robustness sweep of the hl-vulkan lowering shim (task #191, the third leg of the
//! executor #188 / GL-shim #189 robustness trilogy).
//!
//! Every test here drives a shim entrypoint with MALFORMED / HOSTILE input — a dangling handle, an
//! out-of-range index/offset, an oversized/zero allocation, an invalid `VkCreateInfo`, an
//! overflow-inducing coordinate, a double-free / use-after-free, or a submit that references a destroyed
//! resource — and asserts the shim:
//!   1. returns the correct typed `GpuError` (→ the honest `VkResult` via
//!      [`vk_result_from_gpu_error`]) OR performs a documented SAFE handling, and
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
use hl_vulkan::result::{self, vk_result_from_gpu_error};
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::Device;

use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{Cmd, GpuError, RecordingSink};

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
/// Allocate → begin a recording primary command buffer.
fn recording_cb(d: &mut Device) -> u64 {
    let cb = record::allocate_command_buffer(d);
    record::begin(d, cb, false).unwrap();
    cb
}

// =====================================================================================================
// oversized / zero allocations — vkAllocateMemory (REGRESSION: over-heap size host-Vec capacity panic)
// =====================================================================================================

#[test]
fn allocate_memory_zero_over_budget_and_u64max_then_valid() {
    let mut d = dev();
    // A zero allocationSize is a spec usage error (VUID-VkMemoryAllocateInfo-allocationSize-00638).
    assert!(matches!(
        create::allocate_memory(&mut d, 0),
        Err(GpuError::Invalid(_))
    ));
    // Over the modeled 8 GiB unified heap → an honest VK_ERROR_OUT_OF_DEVICE_MEMORY, NOT a fake success.
    let over = d.physical_device.memory_heap_bytes + 1;
    let err = create::allocate_memory(&mut d, over).unwrap_err();
    assert!(matches!(err, GpuError::ResourceLimit(_)));
    assert_eq!(
        vk_result_from_gpu_error(&err),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
    // `u64::MAX` previously capacity-overflow-panicked `vec![0u8; size as usize]` in the host — now a
    // truthful error before any host allocation is attempted.
    assert!(create::allocate_memory(&mut d, u64::MAX).is_err());
    // A valid allocation after every abuse still works.
    let mem = create::allocate_memory(&mut d, 4096).unwrap();
    assert!(d.memories.contains_key(&mem));
}

// =====================================================================================================
// vkMapMemory out-of-range write (REGRESSION: `offset as usize + len` add-overflow panic)
// =====================================================================================================

#[test]
fn write_mapped_offset_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mem = create::allocate_memory(&mut d, 16).unwrap();
    // `u64::MAX` offset previously overflow-panicked `offset as usize + bytes.len()`; now OutOfBounds.
    assert!(matches!(
        create::write_mapped(&mut d, mem, u64::MAX, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        create::write_mapped(&mut d, mem, u64::MAX - 2, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    // A plainly out-of-range (non-overflowing) write is also OutOfBounds.
    assert!(matches!(
        create::write_mapped(&mut d, mem, 14, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    // Unknown memory is a typed Invalid, never a panic.
    assert!(matches!(
        create::write_mapped(&mut d, 0xdead, 0, &[0u8; 4]),
        Err(GpuError::Invalid(_))
    ));
    // A valid in-range write still works.
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(&d.memories.get(&mem).unwrap().data[..4], &[1, 2, 3, 4]);
}

// =====================================================================================================
// vkCmdCopyImage / vkCmdBlitImage coordinate overflow (REGRESSION: `origin + extent` u32 add-overflow)
// =====================================================================================================

#[test]
fn copy_image_origin_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let b = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = recording_cb(&mut d);
    // An origin near `u32::MAX` previously overflow-panicked the `origin + extent > dim` bounds check.
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (u32::MAX, 0), (0, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (0, 0), (u32::MAX, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (0, u32::MAX), (0, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    // A valid in-bounds copy still records.
    assert!(record::cmd_copy_image(&mut d, cb, a, b, (0, 0), (0, 0), (4, 4)).is_ok());
}

#[test]
fn blit_image_extent_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let b = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = recording_cb(&mut d);
    // Origin/extent near `u32::MAX` previously overflow-panicked the src/dst bounds checks.
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (u32::MAX, 0),
            (4, 4),
            (0, 0),
            (4, 4),
            false
        ),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (0, 0),
            (4, 4),
            (0, u32::MAX),
            (4, 4),
            true
        ),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (4, 4),
            (u32::MAX, 1),
            (0, 0),
            (4, 4),
            false
        ),
        Err(GpuError::OutOfBounds)
    ));
    // A valid blit still records.
    assert!(record::cmd_blit_image(&mut d, cb, a, b, (0, 0), (4, 4), (0, 0), (8, 8), true).is_ok());
}

// =====================================================================================================
// push-constant offset/size overflow (REGRESSION: `resize` to multiple GiB aborts the host)
// =====================================================================================================

#[test]
fn push_constants_out_of_range_rejected_then_valid() {
    let mut d = dev();
    let cb = recording_cb(&mut d);
    // offset+size past `maxPushConstantsSize` (4096) previously resized the block to ~4 GiB and aborted.
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0xFFFF_FFF0, &[0u8; 16]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0, &[0u8; 8192]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 4096, &[0u8; 4]),
        Err(GpuError::Invalid(_))
    ));
    // A valid push within the range still records at its offset.
    record::cmd_push_constants(&mut d, cb, 0, &[7u8; 128]).unwrap();
    assert_eq!(
        d.command_buffers.get(&cb).unwrap().push_constants.len(),
        128
    );
}

// =====================================================================================================
// vkCmdBindDescriptorSets firstSet overflow (REGRESSION: `first_set + i` u32 add-overflow)
// =====================================================================================================

#[test]
fn bind_descriptor_sets_first_set_overflow_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let layout = create::create_descriptor_set_layout(&mut d, vec![]);
    let pool = create::create_descriptor_pool(&mut d, 4);
    let sa = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let sb = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let cb = recording_cb(&mut d);
    // firstSet == u32::MAX with >1 set previously overflow-panicked `first_set + i as u32`. It now
    // saturates (a documented safe handling — a real firstSet is bounded by maxBoundDescriptorSets).
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, u32::MAX, &[sa, sb], &[]).unwrap();
    // An unknown set in the batch is skipped (not a panic); the valid `sa` at position 1 lands at set 1.
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[0xdead, sa], &[]).unwrap();
    let sets: Vec<u32> = s
        .commands()
        .filter_map(|c| match c {
            Cmd::CreateBindGroup(_, desc) => Some(desc.set),
            _ => None,
        })
        .collect();
    // The two u32::MAX-saturated binds, then the valid `sa` at set index 1 (the 0xdead at index 0 skipped).
    assert_eq!(sets, vec![u32::MAX, u32::MAX, 1]);
}

// =====================================================================================================
// vkCmdSet*EXT per-attachment array out-of-range (REGRESSION: multi-GiB `resize` aborts the host)
// =====================================================================================================

#[test]
fn dynamic_attachment_array_out_of_range_rejected_then_valid() {
    let mut d = dev();
    let cb = recording_cb(&mut d);
    // `first` near u32::MAX previously resized the state vector to multiple GiB and aborted the host.
    let r = record::set_dynamic_attachment_array(&mut d, cb, u32::MAX, &[1u32], |ds| {
        &mut ds.color_blend_enables
    });
    assert!(matches!(r, Err(GpuError::Invalid(_))));
    let r2 = record::set_dynamic_attachment_array(&mut d, cb, 4, &[1u32; 8], |ds| {
        &mut ds.color_write_masks
    });
    assert!(matches!(r2, Err(GpuError::Invalid(_))));
    // A valid attachment range (within maxColorAttachments == 8) still records.
    record::set_dynamic_attachment_array(&mut d, cb, 0, &[1, 0], |ds| &mut ds.color_blend_enables)
        .unwrap();
    assert_eq!(
        d.command_buffers
            .get(&cb)
            .unwrap()
            .dynamic
            .color_blend_enables,
        vec![1, 0]
    );
}

// =====================================================================================================
// query-result stride overflow (REGRESSION: `count*stride` u64 overflow → multi-EiB Vec / `i*stride` panic)
// =====================================================================================================

#[test]
fn copy_query_pool_results_hostile_stride_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap(); // TIMESTAMP-ish, 4 slots
    let dst = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let cb = recording_cb(&mut d);
    // A hostile `stride` near u64::MAX previously made `count * stride.max(per)` overflow and later
    // aborted the host on a multi-EiB `vec![0u8; dst_size]`; now a truthful OutOfBounds.
    assert!(matches!(
        record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 4, dst, 0, u64::MAX, false, false),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_query_pool_results(
            &mut d,
            cb,
            pool,
            0,
            2,
            dst,
            0,
            u64::MAX / 2,
            true,
            true
        ),
        Err(GpuError::OutOfBounds)
    ));
    // A valid copy (span fits the 256-byte dst) still records.
    record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 4, dst, 0, 4, false, false).unwrap();
}

#[test]
fn get_query_pool_results_hostile_stride_does_not_panic_then_valid() {
    let mut d = dev();
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap();
    let mut out = [0u8; 32];
    // A hostile `stride` near u64::MAX previously overflow-panicked `i * stride as usize` for count>1.
    // Elements that land outside `out` are simply skipped — no panic, returns a defined readiness bool.
    let _ = sync::get_query_pool_results(
        &d,
        pool,
        0,
        4,
        &mut out,
        u64::MAX,
        false,
        true,
        false,
        false,
    )
    .unwrap();
    let _ = sync::get_query_pool_results(
        &d,
        pool,
        0,
        3,
        &mut out,
        u64::MAX / 2,
        true,
        false,
        true,
        false,
    )
    .unwrap();
    // A valid readback (stride 8, availability) still succeeds.
    let mut ok = [0u8; 32];
    let _ =
        sync::get_query_pool_results(&d, pool, 0, 2, &mut ok, 8, false, true, true, false).unwrap();
}

// =====================================================================================================
// invalid VkCreateInfo — zero / oversized image extent, bad format, garbage usage
// =====================================================================================================

#[test]
fn create_image_zero_oversized_extent_bad_format_and_usage() {
    let mut d = dev();
    let mut s = sink();
    // Zero extent is a spec violation → Invalid.
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            0,
            4,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            4,
            0,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    // An extent past maxImageDimension2D (16384) cannot be created → Invalid, never a fake success.
    let big = d.physical_device.limits.max_image_dimension_2d + 1;
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            big,
            4,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    // A bad/unsupported VkFormat folds to Rgba8Unorm (documented bounded translation), no panic.
    let img = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        0xDEAD_BEEF,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    assert_eq!(
        d.images.get(&img).unwrap().format,
        TextureFormat::Rgba8Unorm
    );
    // Garbage usage bits: unknown bits are ignored (known bits translated), no panic.
    let img2 = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        0xFFFF_FFFF,
        1,
    )
    .unwrap();
    assert!(d.images.contains_key(&img2));
}

// =====================================================================================================
// invalid render-pass attachments at begin (unknown color/depth image → rejected, records nothing)
// =====================================================================================================

#[test]
fn begin_render_pass_bad_attachments_reject_and_record_nothing() {
    let mut d = dev();
    let mut s = sink();
    let good = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    // Classic path: an unknown color image is rejected up front.
    let cb = recording_cb(&mut d);
    assert!(matches!(
        record::cmd_begin_render_pass(&mut d, cb, 0xdead, [0.0; 4], true, None),
        Err(GpuError::Invalid(_))
    ));
    // Classic path: a valid color but an unknown depth attachment is rejected (attachment mismatch), and
    // nothing is recorded (the resolve fails BEFORE the encoder push).
    let depth = record::RenderingDepthAttachment {
        image: 0xdead,
        clear_depth: 1.0,
        load_clear: true,
    };
    assert!(matches!(
        record::cmd_begin_render_pass(&mut d, cb, good, [0.0; 4], true, Some(depth)),
        Err(GpuError::Invalid(_))
    ));
    assert!(d.command_buffers.get(&cb).unwrap().enc.is_empty());
    // Dynamic-rendering path: a mix of a valid + an unknown color attachment is rejected atomically.
    let mix = [
        record::RenderingColorAttachment {
            image: good,
            clear: [0.0; 4],
            load_clear: true,
            store: true,
        },
        record::RenderingColorAttachment {
            image: 0xdead,
            clear: [0.0; 4],
            load_clear: true,
            store: true,
        },
    ];
    assert!(matches!(
        record::cmd_begin_rendering(&mut d, cb, &mix, None),
        Err(GpuError::Invalid(_))
    ));
    assert!(d.command_buffers.get(&cb).unwrap().enc.is_empty());
    // A valid begin still records exactly one BeginRenderPass.
    record::cmd_begin_render_pass(&mut d, cb, good, [1.0; 4], true, None).unwrap();
    assert!(!d.command_buffers.get(&cb).unwrap().enc.is_empty());
}

// =====================================================================================================
// dangling / never-created handles across destroy / bind / cmd entrypoints
// =====================================================================================================

#[test]
fn dangling_handles_across_entrypoints_are_typed_errors_or_safe_noops() {
    let mut d = dev();
    let mut s = sink();
    let cb = recording_cb(&mut d);
    // Bind / cmd calls against never-created handles → typed Invalid (no panic).
    assert!(matches!(
        record::cmd_bind_pipeline(&mut d, cb, 0xdead),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_bind_index_buffer(&mut d, cb, 0xdead, 0, 1),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_copy_buffer(&mut d, cb, 0xdead, 0xbeef, 0, 0, 4),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::create_compute_pipeline(&mut d, &mut s, 0xdead, "main"),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::image_subresource_layout(&d, 0xdead),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        present::create_swapchain(&mut d, &mut s, 0xdead, 2),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        present::get_swapchain_images(&d, 0xdead),
        Err(GpuError::Invalid(_))
    ));
    // Destroy of a never-created handle is a defined safe no-op (VK_NULL_HANDLE semantics).
    create::destroy_buffer(&mut d, &mut s, 0xdead).unwrap();
    create::destroy_pipeline_cache(&mut d, 0xdead);
    sync::destroy_event(&mut d, 0xdead);
    sync::destroy_semaphore(&mut d, 0xdead);
    sync::destroy_query_pool(&mut d, 0xdead);
    // A valid resource + command still works after the barrage.
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap();
}

// =====================================================================================================
// out-of-range descriptor indices + vertex/index buffer offsets beyond the allocation (safe forward)
// =====================================================================================================

#[test]
fn out_of_range_indices_and_offsets_do_not_panic_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(
        &mut d,
        &mut s,
        vk_buffer_usage::VERTEX_BUFFER | vk_buffer_usage::INDEX_BUFFER,
        16,
    )
    .unwrap();
    let ir = buf_ir(&d, buf);
    let cb = recording_cb(&mut d);
    // A vertex/index-buffer offset far beyond the 16-byte allocation is forwarded to the IR (the shim is a
    // thin lowering seam — the executor validates the fetch); it must NOT panic or corrupt the recording.
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, u64::MAX).unwrap();
    record::cmd_bind_index_buffer(&mut d, cb, buf, u64::MAX, 1).unwrap();
    // A huge descriptor binding index is just a map key (no panic); the write is retained.
    let layout = create::create_descriptor_set_layout(&mut d, vec![]);
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, u32::MAX, buf, 0, 16).unwrap();
    assert_eq!(
        d.descriptor_sets.get(&set).unwrap().buffers.get(&u32::MAX),
        Some(&(buf, 0, 16))
    );
    // A valid, in-range bind still records against the real ir id.
    record::cmd_bind_vertex_buffer(&mut d, cb, 1, buf, 0).unwrap();
    use hl_gpu::protocol::model::command::Enc;
    assert!(d
        .command_buffers
        .get(&cb)
        .unwrap()
        .enc
        .iter()
        .any(|e| matches!(e, Enc::SetVertexBuffer { buffer, offset: 0, .. } if *buffer == ir)));
}

// =====================================================================================================
// double-free / use-after-free of handles (survives; use-after-free is a typed error)
// =====================================================================================================

#[test]
fn double_free_and_use_after_free_survive() {
    let mut d = dev();
    // Event: double-destroy is a no-op; a use-after-free is a typed Invalid.
    let e = sync::create_event(&mut d);
    sync::destroy_event(&mut d, e);
    sync::destroy_event(&mut d, e); // double free — no panic
    assert!(matches!(
        sync::set_event(&mut d, e, true),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        sync::event_status(&d, e),
        Err(GpuError::Invalid(_))
    ));
    // Pipeline cache: double-destroy no-op; use-after-free → Invalid.
    let c = create::create_pipeline_cache(&mut d, &[]);
    create::destroy_pipeline_cache(&mut d, c);
    create::destroy_pipeline_cache(&mut d, c);
    assert!(matches!(
        create::get_pipeline_cache_data(&d, c),
        Err(GpuError::Invalid(_))
    ));
    // Descriptor-update template: double-destroy no-op.
    let t = create::create_descriptor_update_template(&mut d, 0, vec![]).unwrap();
    create::destroy_descriptor_update_template(&mut d, t);
    create::destroy_descriptor_update_template(&mut d, t);
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, 0, t, &[]),
        Err(GpuError::Invalid(_))
    ));
    // Fresh objects of each kind still work.
    let e2 = sync::create_event(&mut d);
    sync::set_event(&mut d, e2, true).unwrap();
    assert!(sync::event_status(&d, e2).unwrap());
}

// =====================================================================================================
// submit a command buffer that references a destroyed resource (survives; no fake state)
// =====================================================================================================

#[test]
fn submit_command_buffer_referencing_destroyed_resource_survives() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap(); // encoder now holds buf's ir id
    record::end(&mut d, cb).unwrap();
    // Destroy the referenced buffer AFTER recording but BEFORE submit — the encoder still names its ir.
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    // The submit must not panic; the frame ships the (now dangling-ir) SetVertexBuffer for the executor
    // to reject — the shim survives and does not fabricate resource state.
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    // A fresh buffer + a fresh command buffer + submit still works.
    let b2 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb2, 0, b2, 0).unwrap();
    record::end(&mut d, cb2).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb2], None).unwrap();
}

// =====================================================================================================
// bad descriptor writes — type mismatch against the set layout (safe handling, no panic)
// =====================================================================================================

#[test]
fn descriptor_type_mismatch_writes_do_not_panic_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let img = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    // A layout whose binding 0 is a COMBINED_IMAGE_SAMPLER — but the app writes a BUFFER to it.
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: 0,
        }],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // Mismatched type writes are recorded into their kind's map (the executor validates types); no panic.
    create::update_descriptor_buffer(&mut d, set, 0, buf, 0, 64).unwrap();
    create::update_descriptor_image(&mut d, set, 0, Some(img), None).unwrap();
    // Binding the mismatched set still produces a bind group without crashing.
    let cb = recording_cb(&mut d);
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[]).unwrap();
    assert!(s.commands().any(|c| matches!(c, Cmd::CreateBindGroup(..))));
    // A correctly-typed write to a matching binding still lowers to a buffer bind entry.
    let layout2 = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        }],
    );
    let pool2 = create::create_descriptor_pool(&mut d, 1);
    let set2 = create::allocate_descriptor_set(&mut d, pool2, layout2, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set2, 0, buf, 0, 64).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set2], &[]).unwrap();
}
