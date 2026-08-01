//! Clear, fill, and update commands.

use core::ffi::c_void;

use hl_vulkan::service::record;
use hl_vulkan::SubresourceRange;

use super::{CommandBuffer, ShimState};
use crate::types::*;

pub extern "C" fn vkCmdClearColorImage(
    command_buffer: *mut c_void,
    image: u64,
    _image_layout: i32,
    p_color: *const c_void,
    range_count: u32,
    p_ranges: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let Some(color) = (unsafe { (p_color as *const VkClearColorValue).as_ref() }) else {
        return;
    };
    // pRanges says WHICH mip levels and array layers to clear. Dropping it made every clear land on the
    // base subresource, so a layered or mipped image was cleared in the wrong place.
    let ranges: Vec<SubresourceRange> = if range_count == 0 || p_ranges.is_null() {
        Vec::new()
    } else {
        unsafe {
            std::slice::from_raw_parts(
                p_ranges as *const VkImageSubresourceRange,
                range_count as usize,
            )
        }
        .iter()
        .map(|range| SubresourceRange {
            base_mip_level: range.base_mip_level,
            level_count: range.level_count,
            base_array_layer: range.base_array_layer,
            layer_count: range.layer_count,
        })
        .collect()
    };
    ShimState::with_device(|device| {
        // Latched, not discarded. `vkCmd*` returns `void`, so a refused recording has exactly one way
        // to reach the application: the latch that `vkEndCommandBuffer` reports. Dropping it made a
        // refusal indistinguishable from a performed clear — `VK_SUCCESS` from end, from the submit
        // and from the fence, and a black image with no evidence anywhere. Measured against a
        // swapchain image, whose presentable images are created without the `COPY_DST` this refuses
        // on; `vkCmdCopyBufferToImage` fails the identical usage check one file over, latches it, and
        // is diagnosable in one run because of it.
        let recorded =
            record::cmd_clear_color_image(device, command_buffer, image, color.float32, &ranges);
        device.latch(command_buffer, recorded);
    });
}

pub extern "C" fn vkCmdClearAttachments(
    command_buffer: *mut c_void,
    attachment_count: u32,
    p_attachments: *const c_void,
    rect_count: u32,
    p_rects: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if attachment_count == 0 || rect_count == 0 || p_attachments.is_null() || p_rects.is_null() {
        return;
    }
    let attachments = unsafe {
        std::slice::from_raw_parts(
            p_attachments as *const VkClearAttachment,
            attachment_count as usize,
        )
    };
    let rects =
        unsafe { std::slice::from_raw_parts(p_rects as *const VkClearRect, rect_count as usize) };
    ShimState::with_device(|device| {
        for attachment in attachments {
            // Mid-pass depth/stencil rectangle clears cannot be represented by the current IR.
            if attachment.aspect_mask & VK_IMAGE_ASPECT_COLOR_BIT == 0 {
                continue;
            }
            for rect in rects {
                let recorded = record::cmd_clear_attachment_rect(
                    device,
                    command_buffer,
                    rect.rect.offset.x.max(0) as u32,
                    rect.rect.offset.y.max(0) as u32,
                    rect.rect.extent.width,
                    rect.rect.extent.height,
                    attachment.clear_value.float32,
                );
                device.latch(command_buffer, recorded);
            }
        }
    });
}

pub extern "C" fn vkCmdFillBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    size: u64,
    data: u32,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_device(|device| {
        let recorded =
            record::cmd_fill_buffer(device, command_buffer, dst_buffer, dst_offset, size, data);
        device.latch(command_buffer, recorded);
    });
}

pub extern "C" fn vkCmdUpdateBuffer(
    command_buffer: *mut c_void,
    dst_buffer: u64,
    dst_offset: u64,
    data_size: u64,
    p_data: *const c_void,
) {
    let Some(command_buffer) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_data.is_null() || data_size == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p_data as *const u8, data_size as usize) };
    ShimState::with_device(|device| {
        let recorded =
            record::cmd_update_buffer(device, command_buffer, dst_buffer, dst_offset, bytes);
        device.latch(command_buffer, recorded);
    });
}

#[cfg(test)]
mod tests {
    //! A refused recording must reach the application, and `vkCmd*` returns `void`.
    //!
    //! The only channel a refused `vkCmdClearColorImage` has is the latch that `vkEndCommandBuffer`
    //! reports, and this entry point used to discard it. The failure was not a wrong pixel:
    //! `vkEndCommandBuffer`, the submit and the fence all returned `VK_SUCCESS` over a clear the
    //! driver had already decided not to perform, and the application saw a black image with no
    //! evidence anywhere that anything had been refused.
    //!
    //! ## What these cases can and cannot reach
    //!
    //! The refusal driven here is an unknown `VkImage`. The refusal that was actually costing frames
    //! is the missing-`COPY_DST` one, and it is NOT reachable from a unit test: creating an image
    //! goes through `create::create_image_geometry`, which submits to the `RemoteCommandSink`, and
    //! there is no host GPU service in-process — `vkCreateImage` returns
    //! `VK_ERROR_INITIALIZATION_FAILED` before any usage check can run. That was measured while
    //! writing these cases, not assumed.
    //!
    //! Both refusals leave `cmd_clear_color_image` by the same `Err` return into the same latch, so
    //! this pins the propagation the defect was in. The usage-specific case is covered by
    //! `../../../e2e/husklet/apps/vk-windowed`, which reaches it against a real driver and reports it
    //! as `swapchain_roundtrip=end-refused` beside an `offscreen_clear=cleared` control.

    use super::*;
    use crate::tests::{recording_command_buffer, test_guard};

    fn clear(command_buffer: *mut c_void, image: u64) {
        let color = VkClearColorValue { float32: [0.25, 0.5, 0.75, 1.0] };
        let range = VkImageSubresourceRange {
            aspect_mask: VK_IMAGE_ASPECT_COLOR_BIT,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        vkCmdClearColorImage(
            command_buffer,
            image,
            0,
            &color as *const _ as *const c_void,
            1,
            &range as *const _ as *const c_void,
        );
    }

    #[test]
    fn a_refused_clear_fails_the_command_buffer() {
        let _guard = test_guard();
        let (command_buffer, _) = recording_command_buffer();
        // A handle no `vkCreateImage` ever minted. `cmd_clear_color_image` refuses it with
        // `unknown VkImage`, which is the same `Err` channel the usage refusal takes.
        clear(command_buffer, 0xDEAD_BEEF);
        assert_ne!(
            crate::compute::vkEndCommandBuffer(command_buffer),
            VK_SUCCESS,
            "vkCmdClearColorImage refused the clear and vkEndCommandBuffer reported success anyway. \
             That is how a refused clear becomes a black frame with no evidence: every status the \
             application can read says the clear happened."
        );
    }

    #[test]
    fn a_command_buffer_with_no_refused_command_ends_successfully() {
        // The positive control. Without it the case above is satisfied by a command buffer that fails
        // for any reason at all, including one this test setup broke itself — a refusal proves nothing
        // without a path that otherwise works.
        let _guard = test_guard();
        let (command_buffer, _) = recording_command_buffer();
        assert_eq!(
            crate::compute::vkEndCommandBuffer(command_buffer),
            VK_SUCCESS,
            "a command buffer that recorded nothing refusable still failed to end, so the case above \
             is measuring a broken setup rather than the latch"
        );
    }
}
