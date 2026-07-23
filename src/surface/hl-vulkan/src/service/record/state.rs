use super::*;

/// `vkCmdSetViewport` (one viewport) — record `Enc::SetViewport`. The viewport transform is applied by
/// the pass/rasterizer that consumes it.
///
/// A Vulkan app may supply a negative-height viewport. Normalize it to the equivalent positive-height
/// rectangle because the host executor rejects negative viewport heights and applies its own Y flip.
#[allow(clippy::too_many_arguments)]
pub fn cmd_set_viewport(
    dev: &mut Device,
    cb: VkCommandBuffer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    min_depth: f32,
    max_depth: f32,
) -> Result<()> {
    let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
    dev.require_recording(cb)?.enc.push(Enc::SetViewport {
        x,
        y,
        w,
        h,
        min_depth,
        max_depth,
    });
    Ok(())
}

/// `vkCmdSetScissor` (one rect) — record `Enc::SetScissor`. A negative `VkRect2D` offset is clamped to 0
/// by the caller (the IR scissor is unsigned).
pub fn cmd_set_scissor(
    dev: &mut Device,
    cb: VkCommandBuffer,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<()> {
    let rec = dev.require_recording(cb)?;
    // Track the current scissor so an open occlusion query counts only the samples this rect admits.
    rec.scissor = Some((x, y, w, h));
    rec.enc.push(Enc::SetScissor { x, y, w, h });
    Ok(())
}

/// `vkCmdSetLineWidth` — record the dynamic line width (honest command state; the fill rasterizer draws
/// no wide lines, so this emits no encoder op — documented in [`DynamicState`]).
pub fn cmd_set_line_width(dev: &mut Device, cb: VkCommandBuffer, line_width: f32) -> Result<()> {
    dev.require_recording(cb)?.dynamic.line_width = line_width;
    Ok(())
}

/// `vkCmdSetDepthBias` — record `(constantFactor, clamp, slopeFactor)` (no depth buffer; no encoder op).
pub fn cmd_set_depth_bias(
    dev: &mut Device,
    cb: VkCommandBuffer,
    constant_factor: f32,
    clamp: f32,
    slope_factor: f32,
) -> Result<()> {
    dev.require_recording(cb)?.dynamic.depth_bias = (constant_factor, clamp, slope_factor);
    Ok(())
}

/// `vkCmdSetBlendConstants` — record the RGBA blend constants (no constant-color blend; no encoder op).
pub fn cmd_set_blend_constants(
    dev: &mut Device,
    cb: VkCommandBuffer,
    constants: [f32; 4],
) -> Result<()> {
    dev.require_recording(cb)?.dynamic.blend_constants = constants;
    Ok(())
}

/// Apply `value` to the stencil-face pair selected by `face_mask` (VkStencilFaceFlags: FRONT = 0x1,
/// BACK = 0x2, FRONT_AND_BACK = 0x3).
fn set_stencil_faces(pair: &mut (u32, u32), face_mask: u32, value: u32) {
    if face_mask & 0x1 != 0 {
        pair.0 = value;
    }
    if face_mask & 0x2 != 0 {
        pair.1 = value;
    }
}

/// `vkCmdSetStencilCompareMask` — record the compare mask for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_compare_mask(
    dev: &mut Device,
    cb: VkCommandBuffer,
    face_mask: u32,
    mask: u32,
) -> Result<()> {
    set_stencil_faces(
        &mut dev.require_recording(cb)?.dynamic.stencil_compare_mask,
        face_mask,
        mask,
    );
    Ok(())
}

/// `vkCmdSetStencilWriteMask` — record the write mask for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_write_mask(
    dev: &mut Device,
    cb: VkCommandBuffer,
    face_mask: u32,
    mask: u32,
) -> Result<()> {
    set_stencil_faces(
        &mut dev.require_recording(cb)?.dynamic.stencil_write_mask,
        face_mask,
        mask,
    );
    Ok(())
}

/// `vkCmdSetStencilReference` — record the reference value for the selected face(s) (no stencil buffer).
pub fn cmd_set_stencil_reference(
    dev: &mut Device,
    cb: VkCommandBuffer,
    face_mask: u32,
    reference: u32,
) -> Result<()> {
    set_stencil_faces(
        &mut dev.require_recording(cb)?.dynamic.stencil_reference,
        face_mask,
        reference,
    );
    Ok(())
}

// ---- extended dynamic state 1/2/3 --------------------------------------------------------------
// The core-promoted `VK_EXT_extended_dynamic_state{,2,3}` `vkCmdSet*` commands set fixed-function
// pipeline state the software color rasterizer does not model (cull mode, depth/stencil test enables,
// blend/logic-op state, ...). Each is recorded verbatim into the command buffer's [`DynamicState`] —
// observable, honest command state — and carries NO encoder op. `set_dynamic` is the single seam the
// shim's extended-dynamic-state bodies mutate through (it enforces the "must be recording" rule).

/// Mutate the recording command buffer's [`DynamicState`] with `f`. The one entry point every extended
/// `vkCmdSet*` records through. Errors if `cb` is not currently recording (the Vulkan rule).
pub fn set_dynamic<R>(
    dev: &mut Device,
    cb: VkCommandBuffer,
    f: impl FnOnce(&mut crate::model::command::DynamicState) -> R,
) -> Result<R> {
    Ok(f(&mut dev.require_recording(cb)?.dynamic))
}

/// Set the extended-stencil-op state for the face(s) selected by `face_mask` (FRONT = 0x1, BACK = 0x2).
/// Helper for `vkCmdSetStencilOp` (`(failOp, passOp, depthFailOp, compareOp)`).
pub fn set_stencil_op(
    dev: &mut Device,
    cb: VkCommandBuffer,
    face_mask: u32,
    ops: (i32, i32, i32, i32),
) -> Result<()> {
    let ds = &mut dev.require_recording(cb)?.dynamic;
    if face_mask & 0x1 != 0 {
        ds.stencil_op_front = ops;
    }
    if face_mask & 0x2 != 0 {
        ds.stencil_op_back = ops;
    }
    Ok(())
}

/// Record a per-attachment extended-dynamic-state array (`vkCmdSetColorBlendEnableEXT` /
/// `vkCmdSetColorWriteMaskEXT` / `vkCmdSetColorWriteEnableEXT`): overwrite `[first, first+values.len())`
/// of `target`, growing it as needed. `select` picks which `DynamicState` vector to write.
pub fn set_dynamic_attachment_array(
    dev: &mut Device,
    cb: VkCommandBuffer,
    first: u32,
    values: &[u32],
    select: impl FnOnce(&mut crate::model::command::DynamicState) -> &mut Vec<u32>,
) -> Result<()> {
    // The written range indexes color attachments, bounded by `maxColorAttachments`. Without this a
    // hostile `first` near `u32::MAX` would `resize` the state vector to multiple GiB and abort the host
    // on the allocation — reject an out-of-range attachment span as a truthful usage error instead.
    let end = first as usize + values.len();
    if end > dev.physical_device.limits.max_color_attachments as usize {
        return Err(GpuError::Invalid(
            "vkCmdSet*EXT: attachment range exceeds maxColorAttachments",
        ));
    }
    let ds = &mut dev.require_recording(cb)?.dynamic;
    let target = select(ds);
    if target.len() < end {
        target.resize(end, 0);
    }
    target[first as usize..end].copy_from_slice(values);
    Ok(())
}
