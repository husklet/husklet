//! Command-buffer replay: turn a validated [`CommandBuffer`]'s encoder ops into real wgpu work.
//!
//! Render and compute passes are executed as forward-scanned `Begin..End` units (a wgpu pass borrows its
//! encoder for its whole lifetime, so it can't straddle the outer op loop); everything else — sub-rect
//! clears, buffer/texture copies, fills — is CPU-mediated through the byte-addressable buffer/texture
//! helpers so wgpu's copy-alignment rules never leak to the protocol boundary. Queue ordering preserves
//! dependencies between ordinary units; only CPU readback and explicit completion fences block the host.
//!
//! The clears, copies, dispatches, and draws here are the executed analogues of the CPU oracle's
//! `submit` (`hl-gpu/src/cpu/executor.rs`) — same ops, same order, now on the GPU.

use hl_gpu::protocol::model::command::{CommandBuffer, Enc};
use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, DepthAttachment, Extent3d, Origin3d, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{Filter, IndexFormat, LoadOp, TextureAspect, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use hl_log::tag;

use crate::convert::Format;
use crate::pipeline::PipelineNative;
use crate::{buffer, fence, texture, WgpuExecutor};

mod vertex;

/// Intersect a GL-style viewport rect `(x, y, w, h)` with the render target `[0, tw] × [0, th]` so it
/// satisfies wgpu's strict `RenderPass::set_viewport` bounds (`x,y >= 0`, `x+w <= tw`, `y+h <= th`, `w,h > 0`
/// — see `wgpu_core::command::render::set_viewport`). Returns the in-bounds sub-rect, or `None` when the
/// intersection is empty (the whole viewport lies outside the target — nothing should rasterize).
///
/// WHY this exists: GL's `glViewport` is only the NDC→window transform and permits a rect that starts
/// negative or overhangs the framebuffer; GL simply lets the framebuffer clip the fragments. wgpu forbids
/// such a rect outright, so forwarding Chrome's legitimate scrolled-layer viewport (`y=-386, h=642` into a
/// 256-tall target) verbatim NACKs the frame and orphans its resources. Intersecting makes the frame VALID
/// and keeps the whole in-bounds path (every non-scrolled draw) pixel-exact.
///
/// The returned correction preserves the original NDC→window transform while wgpu rasterizes through the
/// legal intersection. Vertex lowering applies `P'.xy = scale * P.xy + bias * P.w`, equivalent to ANGLE's
/// driver-uniform technique.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Viewport {
    clipped: (f32, f32, f32, f32),
    correction: [f32; 4],
}

impl Viewport {
    fn new(x: f32, y: f32, w: f32, h: f32, tw: u32, th: u32) -> Option<Self> {
        let (tw, th) = (tw as f32, th as f32);
        let x0 = x.max(0.0);
        let y0 = y.max(0.0);
        let x1 = (x + w).min(tw);
        let y1 = (y + h).min(th);
        let cw = x1 - x0;
        let ch = y1 - y0;
        if cw.is_finite() && ch.is_finite() && cw > 0.0 && ch > 0.0 {
            Some(Self {
                clipped: (x0, y0, cw, ch),
                correction: [
                    w / cw,
                    h / ch,
                    (2.0 * (x - x0) + w - cw) / cw,
                    (2.0 * (y0 - y) + ch - h) / ch,
                ],
            })
        } else {
            None
        }
    }

    fn full() -> Self {
        Self {
            clipped: (0.0, 0.0, 0.0, 0.0),
            correction: [1.0, 1.0, 0.0, 0.0],
        }
    }
}

impl WgpuExecutor {
    pub(crate) fn submit_cb(
        &mut self,
        res: &mut SessionResources,
        cb: &CommandBuffer,
    ) -> Result<()> {
        // Pass-local scopes attribute errors to the offending pass. This outer scope also covers the
        // deferred native submission, because wgpu performs some validation only when an encoder is
        // submitted.
        self.with_validation_scope(|executor| executor.submit_cb_inner(res, cb))
    }

    fn submit_cb_inner(&mut self, res: &mut SessionResources, cb: &CommandBuffer) -> Result<()> {
        let ops = &cb.encoder;
        let mut i = 0;
        let mut native = None;
        while i < ops.len() {
            let native_buffer_copy = matches!(
                &ops[i],
                Enc::CopyBufferToBuffer {
                    src,
                    src_offset,
                    dst,
                    dst_offset,
                    size,
                } if src != dst
                    && *size != 0
                    && src_offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                    && dst_offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                    && size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            );
            let potential_native_texture_copy = matches!(
                &ops[i],
                Enc::CopyTextureToTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } if src != dst
                    && src_sub.mip == 0
                    && src_sub.layer == 0
                    && src_sub.aspect == TextureAspect::All
                    && dst_sub.mip == 0
                    && dst_sub.layer == 0
                    && dst_sub.aspect == TextureAspect::All
                    && src_origin.z == 0
                    && dst_origin.z == 0
                    && extent.width != 0
                    && extent.height != 0
                    && extent.depth == 1
            );
            let potential_native_texture_upload = matches!(
                &ops[i],
                Enc::CopyBufferToTexture {
                    src_offset,
                    width,
                    height,
                    ..
                } if src_offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                    && *width != 0
                    && *height != 0
            );
            if !matches!(&ops[i], Enc::BeginRenderPass { .. } | Enc::BeginComputePass)
                && !native_buffer_copy
                && !potential_native_texture_copy
                && !potential_native_texture_upload
            {
                self.submit_encoded(&mut native);
            }
            match &ops[i] {
                Enc::BeginRenderPass { color, depth } => {
                    let end = find_end(ops, i, Enc::EndRenderPass)?;
                    let encoder = native.get_or_insert_with(|| {
                        self.gpu
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("hl-command-buffer"),
                            })
                    });
                    let started = std::time::Instant::now();
                    self.run_render_pass(res, color, depth.as_ref(), &ops[i + 1..end], encoder)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.render_passes.add(started.elapsed());
                    }
                    i = end + 1;
                }
                Enc::BeginComputePass => {
                    let end = find_end(ops, i, Enc::EndComputePass)?;
                    let encoder = native.get_or_insert_with(|| {
                        self.gpu
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("hl-command-buffer"),
                            })
                    });
                    let started = std::time::Instant::now();
                    self.run_compute_pass(res, &ops[i + 1..end], encoder)?;
                    if let Some(profile) = self.profile.borrow_mut().as_mut() {
                        profile.compute_passes.add(started.elapsed());
                    }
                    i = end + 1;
                }
                Enc::ClearRect {
                    texture,
                    x,
                    y,
                    w,
                    h,
                    color,
                } => {
                    let (fmt, tw, th) = {
                        let t = texture::WgpuTexture::get(res, *texture)?;
                        (t.format, t.width, t.height)
                    };
                    // Clamp the rect to the texture, exactly as the CPU oracle's `clear_rect` does: a rect
                    // that runs past the texture edge fills ONLY the covered sub-rectangle. Without this the
                    // raw `x,y,w,h` would be handed to `Queue::write_texture`, whose bounds validation
                    // rejects an over-hang (a hard wgpu error) — where the oracle silently clamps. The
                    // protocol/runtime `validate` does not bounds-check `ClearRect`, so an over-hanging rect
                    // is a legal command; the two backends must handle it identically. An empty clamped rect
                    // is a no-op.
                    let x0 = (*x).min(tw);
                    let y0 = (*y).min(th);
                    let cw = x.saturating_add(*w).min(tw).saturating_sub(x0);
                    let ch = y.saturating_add(*h).min(th).saturating_sub(y0);
                    if cw != 0 && ch != 0 {
                        let texel = Format::from(fmt).clear_texel(*color)?;
                        let mut data = Vec::with_capacity(texel.len() * cw as usize * ch as usize);
                        for _ in 0..(cw as usize * ch as usize) {
                            data.extend_from_slice(&texel);
                        }
                        self.write_region(res, *texture, x0, y0, 0, cw, ch, 1, 0, &data)?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToBuffer {
                    src,
                    src_offset,
                    dst,
                    dst_offset,
                    size,
                } => {
                    if native_buffer_copy {
                        let source = buffer::WgpuBuffer::get(res, *src)?;
                        let destination = buffer::WgpuBuffer::get(res, *dst)?;
                        let source_end = src_offset
                            .checked_add(*size)
                            .filter(|end| *end <= source.size)
                            .ok_or(GpuError::OutOfBounds)?;
                        let destination_end = dst_offset
                            .checked_add(*size)
                            .filter(|end| *end <= destination.size)
                            .ok_or(GpuError::OutOfBounds)?;
                        debug_assert!(source_end >= *src_offset);
                        debug_assert!(destination_end >= *dst_offset);
                        native
                            .get_or_insert_with(|| {
                                self.gpu.device.create_command_encoder(
                                    &wgpu::CommandEncoderDescriptor {
                                        label: Some("hl-command-buffer"),
                                    },
                                )
                            })
                            .copy_buffer_to_buffer(
                                &source.buffer,
                                *src_offset,
                                &destination.buffer,
                                *dst_offset,
                                *size,
                            );
                    } else {
                        let bytes = self.read_bytes(res, *src, *src_offset, *size as usize)?;
                        self.write_bytes(res, *dst, *dst_offset, &bytes)?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToTextureRegion {
                    src,
                    src_offset,
                    bytes_per_row,
                    rows_per_image,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    self.submit_encoded(&mut native);
                    if dst_sub.aspect != TextureAspect::All {
                        return Err(GpuError::Unsupported(
                            "buffer to texture region: depth/stencil aspects",
                        ));
                    }
                    let format = {
                        let texture = texture::WgpuTexture::get(res, *dst)?;
                        Format::from(texture.format)
                    };
                    let (tight_row, image_rows) =
                        format.copy_layout(extent.width, extent.height)?;
                    let source_stride = if *bytes_per_row == 0 {
                        tight_row
                    } else {
                        *bytes_per_row
                    };
                    if source_stride < tight_row {
                        return Err(GpuError::OutOfBounds);
                    }
                    let source_rows_per_image = if *rows_per_image == 0 {
                        image_rows
                    } else {
                        *rows_per_image
                    };
                    if source_rows_per_image < image_rows {
                        return Err(GpuError::OutOfBounds);
                    }
                    let rows = source_rows_per_image
                        .checked_mul(extent.depth.saturating_sub(1))
                        .and_then(|rows| rows.checked_add(image_rows))
                        .ok_or(GpuError::OutOfBounds)?;
                    let span = rows
                        .saturating_sub(1)
                        .checked_mul(source_stride)
                        .and_then(|bytes| bytes.checked_add(tight_row))
                        .ok_or(GpuError::OutOfBounds)?;
                    let source = self.read_bytes(
                        res,
                        *src,
                        *src_offset,
                        usize::try_from(span).map_err(|_| GpuError::OutOfBounds)?,
                    )?;
                    let tight = if source_stride == tight_row && source_rows_per_image == image_rows
                    {
                        source
                    } else {
                        let mut compact = Vec::with_capacity(
                            usize::try_from(tight_row)
                                .ok()
                                .and_then(|row| row.checked_mul(image_rows as usize))
                                .and_then(|plane| plane.checked_mul(extent.depth as usize))
                                .ok_or(GpuError::OutOfBounds)?,
                        );
                        for layer in 0..extent.depth as usize {
                            for row in 0..image_rows as usize {
                                let offset = (layer * source_rows_per_image as usize + row)
                                    * source_stride as usize;
                                compact.extend_from_slice(
                                    &source[offset..offset + tight_row as usize],
                                );
                            }
                        }
                        compact
                    };
                    let layer = dst_sub
                        .layer
                        .checked_add(dst_origin.z)
                        .ok_or(GpuError::OutOfBounds)?;
                    self.write_region(
                        res,
                        *dst,
                        dst_origin.x,
                        dst_origin.y,
                        layer,
                        extent.width,
                        extent.height,
                        extent.depth,
                        dst_sub.mip,
                        &tight,
                    )?;
                    i += 1;
                }
                Enc::CopyTextureToBufferRegion {
                    src,
                    src_sub,
                    src_origin,
                    extent,
                    dst,
                    dst_offset,
                    bytes_per_row,
                    rows_per_image,
                } => {
                    self.submit_encoded(&mut native);
                    if src_sub.aspect != TextureAspect::All {
                        return Err(GpuError::Unsupported(
                            "texture to buffer region: depth/stencil aspects",
                        ));
                    }
                    let format = {
                        let texture = texture::WgpuTexture::get(res, *src)?;
                        Format::from(texture.format)
                    };
                    let (tight_row, image_rows) =
                        format.copy_layout(extent.width, extent.height)?;
                    let destination_stride = if *bytes_per_row == 0 {
                        tight_row
                    } else {
                        *bytes_per_row
                    };
                    if destination_stride < tight_row {
                        return Err(GpuError::OutOfBounds);
                    }
                    let layer = src_sub
                        .layer
                        .checked_add(src_origin.z)
                        .ok_or(GpuError::OutOfBounds)?;
                    let tight = self.read_region(
                        res,
                        *src,
                        src_origin.x,
                        src_origin.y,
                        layer,
                        extent.width,
                        extent.height,
                        extent.depth,
                        src_sub.mip,
                    )?;
                    let destination_rows_per_image = if *rows_per_image == 0 {
                        image_rows
                    } else {
                        *rows_per_image
                    };
                    if destination_rows_per_image < image_rows {
                        return Err(GpuError::OutOfBounds);
                    }
                    for layer in 0..extent.depth as usize {
                        for row in 0..image_rows as usize {
                            let destination_row = layer * destination_rows_per_image as usize + row;
                            let source_row = layer * image_rows as usize + row;
                            self.write_bytes(
                                res,
                                *dst,
                                dst_offset
                                    .checked_add(
                                        (destination_row as u64) * u64::from(destination_stride),
                                    )
                                    .ok_or(GpuError::OutOfBounds)?,
                                &tight[source_row * tight_row as usize
                                    ..(source_row + 1) * tight_row as usize],
                            )?;
                        }
                    }
                    i += 1;
                }
                Enc::CopyTextureToBuffer {
                    src,
                    mip,
                    width,
                    height,
                    dst,
                    dst_offset,
                    bytes_per_row,
                } => {
                    let (format, tw, th, mips, samples) = {
                        let t = texture::WgpuTexture::get(res, *src)?;
                        (
                            Format::from(t.format),
                            t.width,
                            t.height,
                            t.mip_levels,
                            t.sample_count,
                        )
                    };
                    if samples != 1 {
                        return Err(GpuError::Unsupported(
                            "copy texture to buffer: multisampled texture must be resolved first",
                        ));
                    }
                    // Honor the `mip` field: the readback below reads THAT level (not silently the base).
                    // An out-of-range mip is a typed `OutOfBounds` (the runtime does not range-check this op).
                    if *mip >= mips {
                        return Err(GpuError::OutOfBounds);
                    }
                    // The copy region must lie inside the SOURCE MIP LEVEL, whose dimensions are the base
                    // extent halved per level (floored at 1). A `width`/`height` past the level edge would
                    // slice past the tight readback plane below (a Rust panic), so guard it into `OutOfBounds`.
                    let lw = (tw >> *mip).max(1);
                    let lh = (th >> *mip).max(1);
                    if *width > lw || *height > lh {
                        return Err(GpuError::OutOfBounds);
                    }
                    let (row, copy_rows) = format.copy_layout(*width, *height)?;
                    let dst_stride = if *bytes_per_row == 0 {
                        row
                    } else {
                        *bytes_per_row
                    };
                    if dst_stride < row {
                        return Err(GpuError::OutOfBounds);
                    }
                    let span = if copy_rows == 0 {
                        0
                    } else {
                        u64::from(copy_rows - 1)
                            .checked_mul(u64::from(dst_stride))
                            .and_then(|bytes| bytes.checked_add(u64::from(row)))
                            .ok_or(GpuError::OutOfBounds)?
                    };
                    let dst_end = dst_offset.checked_add(span).ok_or(GpuError::OutOfBounds)?;
                    if dst_end > buffer::WgpuBuffer::get(res, *dst)?.size {
                        return Err(GpuError::OutOfBounds);
                    }

                    // Preserve encoder order across the native-copy optimization. Render/compute passes
                    // accumulated above are not visible to the queue until `native` is submitted; issuing
                    // the texture copy first would read the texture before those passes produced it.
                    self.submit_encoded(&mut native);

                    // Common presentation targets are four-byte BGRA/RGBA texels, so every row remains
                    // four-byte aligned even when its width is not a 256-byte multiple. Copy the texture
                    // into a padded GPU buffer, then compact its rows into the protocol buffer in the SAME
                    // encoder. This is one queue submission and no host wait. Byte-addressable layouts that
                    // violate WGPU's buffer-copy alignment retain the exact CPU fallback below.
                    if self.copy_texture_to_buffer_native(
                        res,
                        *src,
                        *mip,
                        *width,
                        *height,
                        copy_rows,
                        *dst,
                        *dst_offset,
                        row,
                        dst_stride,
                    )? {
                        i += 1;
                        continue;
                    }

                    hl_log::hl_count!(hl_log::tag::PRESENT, "copy_texture_buffer_fallback");
                    hl_log::hl_add!(
                        hl_log::tag::PRESENT,
                        "copy_texture_buffer_fallback_bytes",
                        u64::from(row) * u64::from(copy_rows)
                    );

                    // The tight readback plane is packed at the MIP LEVEL's width, not the copy region's
                    // width — the source row stride is `mip_width*bpt`, so a sub-region copy (width < level
                    // width) advances by the full plane stride, exactly as the CPU oracle does at level 0.
                    let (src_stride, _) = format.copy_layout(lw, lh)?;
                    let src_stride = src_stride as usize;
                    let plane = self.read_texture_tight_mip(res, *src, *mip)?;
                    let row = row as usize;
                    let dst_stride = dst_stride as usize;
                    for r in 0..copy_rows as usize {
                        let s = r * src_stride;
                        let d = *dst_offset + (r * dst_stride) as u64;
                        self.write_bytes(res, *dst, d, &plane[s..s + row])?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToTexture {
                    src,
                    src_offset,
                    bytes_per_row,
                    dst,
                    mip,
                    width,
                    height,
                } => {
                    let (row, image_rows, dst_depth, destination) = {
                        let t = texture::WgpuTexture::get(res, *dst)?;
                        // The destination region (`mip`, `width`, `height`) must fit the texture: an
                        // out-of-range mip or a `width`/`height` overhanging the mip level would be handed
                        // to `queue.write_texture`, whose bounds validation is a HARD wgpu error (its
                        // uncaptured-error handler panics). The runtime does not range-check this op, so
                        // guard it into a typed error here. Mip-level extent is the base extent halved per
                        // level, floored at 1 (the WebGPU mip pyramid).
                        if *mip >= t.mip_levels {
                            return Err(GpuError::OutOfBounds);
                        }
                        let lw = (t.width >> *mip).max(1);
                        let lh = (t.height >> *mip).max(1);
                        if *width > lw || *height > lh {
                            return Err(GpuError::OutOfBounds);
                        }
                        if t.sample_count != 1 {
                            return Err(GpuError::Unsupported(
                                "copy buffer to texture: multisampled texture cannot be uploaded",
                            ));
                        }
                        let (row, image_rows) =
                            Format::from(t.format).copy_layout(*width, *height)?;
                        (row, image_rows, t.depth, t.texture.clone())
                    };
                    // `bytes_per_row == 0` means the source rows are tightly packed (the oracle convention).
                    let src_stride = if *bytes_per_row == 0 {
                        row
                    } else {
                        *bytes_per_row
                    };
                    if src_stride < row {
                        return Err(GpuError::OutOfBounds);
                    }
                    // A 3D destination has no z/depth field on this op, so the copy fills the WHOLE volume:
                    // the source holds `width*height*depth` tightly-stacked slices (rows advance `height`
                    // per slice). A plain 2D texture keeps `depth == 1`, i.e. the original single-plane copy.
                    let rows = image_rows
                        .checked_mul(dst_depth)
                        .ok_or(GpuError::OutOfBounds)?;
                    let span = rows
                        .saturating_sub(1)
                        .checked_mul(src_stride)
                        .and_then(|bytes| bytes.checked_add(row))
                        .ok_or(GpuError::OutOfBounds)?;
                    let source = buffer::WgpuBuffer::get(res, *src)?;
                    src_offset
                        .checked_add(u64::from(span))
                        .filter(|end| *end <= source.size)
                        .ok_or(GpuError::OutOfBounds)?;

                    // Keep representable uploads entirely on the GPU. WebGPU requires a four-byte source
                    // offset and, when more than one row is copied, a 256-byte row pitch. The final row may
                    // be tight, so the exact protocol span validated above remains authoritative.
                    let native_compatible = potential_native_texture_upload
                        && (rows == 1
                            || u64::from(src_stride)
                                .is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64));
                    if native_compatible {
                        native
                            .get_or_insert_with(|| {
                                self.gpu.device.create_command_encoder(
                                    &wgpu::CommandEncoderDescriptor {
                                        label: Some("hl-command-buffer"),
                                    },
                                )
                            })
                            .copy_buffer_to_texture(
                                wgpu::TexelCopyBufferInfo {
                                    buffer: &source.buffer,
                                    layout: wgpu::TexelCopyBufferLayout {
                                        offset: *src_offset,
                                        bytes_per_row: (rows > 1).then_some(src_stride),
                                        rows_per_image: (dst_depth > 1).then_some(image_rows),
                                    },
                                },
                                wgpu::TexelCopyTextureInfo {
                                    texture: &destination,
                                    mip_level: *mip,
                                    origin: wgpu::Origin3d::ZERO,
                                    aspect: wgpu::TextureAspect::All,
                                },
                                wgpu::Extent3d {
                                    width: *width,
                                    height: *height,
                                    depth_or_array_layers: dst_depth,
                                },
                            );
                        i += 1;
                        continue;
                    }

                    // A tight multi-row layout often has a pitch below WebGPU's 256-byte texture-copy
                    // alignment (glyph atlases are commonly 64/128/160 bytes wide). Repack those rows on
                    // the GPU: ordinary buffer copies retain only the four-byte rule, so copy each logical
                    // row into a transient 256-byte-pitched buffer and upload that buffer in this same
                    // command encoder. Padding is never observed by the destination texture.
                    let staging_compatible = potential_native_texture_upload
                        && rows > 1
                        && u64::from(row).is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                        && u64::from(src_stride).is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
                    if staging_compatible {
                        let padded_row = row
                            .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                            .ok_or(GpuError::OutOfBounds)?;
                        let staging_size = u64::from(padded_row)
                            .checked_mul(u64::from(rows))
                            .ok_or(GpuError::OutOfBounds)?;
                        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("hl-native-texture-upload"),
                            size: staging_size,
                            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        let encoder = native.get_or_insert_with(|| {
                            self.gpu.device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("hl-command-buffer"),
                                },
                            )
                        });
                        for source_row in 0..rows {
                            encoder.copy_buffer_to_buffer(
                                &source.buffer,
                                src_offset
                                    .checked_add(u64::from(source_row) * u64::from(src_stride))
                                    .ok_or(GpuError::OutOfBounds)?,
                                &staging,
                                u64::from(source_row) * u64::from(padded_row),
                                u64::from(row),
                            );
                        }
                        encoder.copy_buffer_to_texture(
                            wgpu::TexelCopyBufferInfo {
                                buffer: &staging,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(padded_row),
                                    rows_per_image: (dst_depth > 1).then_some(image_rows),
                                },
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &destination,
                                mip_level: *mip,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: *width,
                                height: *height,
                                depth_or_array_layers: dst_depth,
                            },
                        );
                        i += 1;
                        continue;
                    }

                    self.submit_encoded(&mut native);
                    // Map the source buffer once, then compact padded rows in host memory. Mapping every
                    // row separately forced one device-wide completion wait per row; Chrome's two small
                    // uploads in the captured frame caused 40 waits and dominated all pass encoding.
                    let source = self.read_bytes(
                        res,
                        *src,
                        *src_offset,
                        usize::try_from(span).map_err(|_| GpuError::OutOfBounds)?,
                    )?;
                    let tight = if src_stride == row {
                        source
                    } else {
                        let capacity = usize::try_from(row)
                            .ok()
                            .and_then(|row| {
                                usize::try_from(rows)
                                    .ok()
                                    .and_then(|rows| row.checked_mul(rows))
                            })
                            .ok_or(GpuError::OutOfBounds)?;
                        let mut tight = Vec::with_capacity(capacity);
                        for r in 0..rows {
                            let offset = r.checked_mul(src_stride).ok_or(GpuError::OutOfBounds)?;
                            let start =
                                usize::try_from(offset).map_err(|_| GpuError::OutOfBounds)?;
                            let end = usize::try_from(
                                offset.checked_add(row).ok_or(GpuError::OutOfBounds)?,
                            )
                            .map_err(|_| GpuError::OutOfBounds)?;
                            tight.extend_from_slice(&source[start..end]);
                        }
                        tight
                    };
                    self.write_region(
                        res, *dst, 0, 0, 0, *width, *height, dst_depth, *mip, &tight,
                    )?;
                    i += 1;
                }
                Enc::CopyTextureToTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    let source = texture::WgpuTexture::get(res, *src)?;
                    let destination = texture::WgpuTexture::get(res, *dst)?;
                    let native_compatible = potential_native_texture_copy
                        && source.format == destination.format
                        && source.sample_count == 1
                        && destination.sample_count == 1
                        && source.depth == 1
                        && destination.depth == 1;
                    if native_compatible {
                        if let Some((bw, bh, _)) = source.format.block_geometry() {
                            let aligned =
                                |origin: &Origin3d, width: u32, height: u32, tw: u32, th: u32| {
                                    origin.x.is_multiple_of(bw)
                                        && origin.y.is_multiple_of(bh)
                                        && (width.is_multiple_of(bw)
                                            || origin.x.checked_add(width) == Some(tw))
                                        && (height.is_multiple_of(bh)
                                            || origin.y.checked_add(height) == Some(th))
                                };
                            if !aligned(
                                src_origin,
                                extent.width,
                                extent.height,
                                source.width,
                                source.height,
                            ) || !aligned(
                                dst_origin,
                                extent.width,
                                extent.height,
                                destination.width,
                                destination.height,
                            ) {
                                return Err(GpuError::Invalid(
                                    "compressed texture copy is not block aligned",
                                ));
                            }
                        }
                        let source_in_bounds = src_origin
                            .x
                            .checked_add(extent.width)
                            .is_some_and(|end| end <= source.width)
                            && src_origin
                                .y
                                .checked_add(extent.height)
                                .is_some_and(|end| end <= source.height);
                        let destination_in_bounds = dst_origin
                            .x
                            .checked_add(extent.width)
                            .is_some_and(|end| end <= destination.width)
                            && dst_origin
                                .y
                                .checked_add(extent.height)
                                .is_some_and(|end| end <= destination.height);
                        if !source_in_bounds || !destination_in_bounds {
                            return Err(GpuError::OutOfBounds);
                        }
                        native
                            .get_or_insert_with(|| {
                                self.gpu.device.create_command_encoder(
                                    &wgpu::CommandEncoderDescriptor {
                                        label: Some("hl-command-buffer"),
                                    },
                                )
                            })
                            .copy_texture_to_texture(
                                wgpu::TexelCopyTextureInfo {
                                    texture: &source.texture,
                                    mip_level: 0,
                                    origin: wgpu::Origin3d {
                                        x: src_origin.x,
                                        y: src_origin.y,
                                        z: 0,
                                    },
                                    aspect: wgpu::TextureAspect::All,
                                },
                                wgpu::TexelCopyTextureInfo {
                                    texture: &destination.texture,
                                    mip_level: 0,
                                    origin: wgpu::Origin3d {
                                        x: dst_origin.x,
                                        y: dst_origin.y,
                                        z: 0,
                                    },
                                    aspect: wgpu::TextureAspect::All,
                                },
                                wgpu::Extent3d {
                                    width: extent.width,
                                    height: extent.height,
                                    depth_or_array_layers: 1,
                                },
                            );
                    } else {
                        self.submit_encoded(&mut native);
                        self.copy_texture_to_texture(
                            res, *src, src_sub, src_origin, *dst, dst_sub, dst_origin, extent,
                        )?;
                    }
                    i += 1;
                }
                Enc::FillBuffer {
                    buffer,
                    offset,
                    size,
                    value,
                } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                    i += 1;
                }
                // Scaled/filtered blit: wgpu has no native image blit, so it is resampled by a
                // textured-triangle draw into the destination rect (see `blit.rs`). This is the executed
                // analogue of the CPU oracle's `blit_texture`.
                Enc::BlitTexture {
                    src,
                    src_sub,
                    src_origin,
                    src_extent,
                    dst,
                    dst_sub,
                    dst_origin,
                    dst_extent,
                    filter,
                } => {
                    self.blit_texture(
                        res, *src, src_sub, src_origin, src_extent, *dst, dst_sub, dst_origin,
                        dst_extent, *filter,
                    )?;
                    i += 1;
                }
                // Multisample resolve: average the multisampled `src`'s samples into single-sample `dst`.
                // wgpu has no standalone resolve command, so it is realized as a zero-draw render pass that
                // LOADs the multisampled color attachment and hands `dst` as its `resolve_target` — the
                // resolve happens at pass end (see `resolve_texture`). This is the executed analogue of the
                // CPU oracle's sample averaging.
                Enc::ResolveTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    self.resolve_texture(
                        res, *src, src_sub, src_origin, *dst, dst_sub, dst_origin, extent,
                    )?;
                    i += 1;
                }
                // Stray state-setters outside a pass cannot occur in a validated command buffer.
                _ => i += 1,
            }
        }
        self.submit_encoded(&mut native);
        if let Some((f, v)) = cb.signal {
            let slot = fence::Fence::schedule(res, f, v)?;
            self.gpu.queue.on_submitted_work_done(move || {
                fence::Fence::signal(&slot, v);
            });
        }
        Ok(())
    }

    fn submit_encoded(&self, encoder: &mut Option<wgpu::CommandEncoder>) {
        let Some(encoder) = encoder.take() else {
            return;
        };
        let diagnostics = hl_log::Logging::global().enabled(
            hl_log::Tags::from(hl_log::tag::PRESENT),
            hl_log::Level::Debug,
        );
        let started = diagnostics.then(std::time::Instant::now);
        self.gpu.queue.submit(Some(encoder.finish()));
        if let Some(started) = started {
            hl_log::hl_debug!(
                hl_log::tag::PRESENT,
                "native_present phase=queue_submit submit_us={}",
                started.elapsed().as_micros()
            );
        }
        if let Some(profile) = self.profile.borrow_mut().as_mut() {
            profile.native_submissions = profile.native_submissions.saturating_add(1);
        }
        #[cfg(test)]
        self.command_submissions
            .set(self.command_submissions.get().saturating_add(1));
    }

    /// Execute a texture-to-buffer copy entirely on the GPU when WGPU's four-byte buffer-copy alignment
    /// can represent the neutral byte layout. Returns `Ok(false)` without mutation when the layout requires
    /// the byte-addressable CPU fallback.
    #[allow(clippy::too_many_arguments)]
    fn copy_texture_to_buffer_native(
        &self,
        res: &SessionResources,
        src: u32,
        mip: u32,
        width: u32,
        height: u32,
        copy_rows: u32,
        dst: u32,
        dst_offset: u64,
        row_bytes: u32,
        dst_stride: u32,
    ) -> Result<bool> {
        const BUFFER_ALIGNMENT: u64 = wgpu::COPY_BUFFER_ALIGNMENT;
        if width == 0 || height == 0 {
            return Ok(true);
        }
        if !dst_offset.is_multiple_of(BUFFER_ALIGNMENT)
            || !u64::from(row_bytes).is_multiple_of(BUFFER_ALIGNMENT)
            || !u64::from(dst_stride).is_multiple_of(BUFFER_ALIGNMENT)
        {
            return Ok(false);
        }

        let source = texture::WgpuTexture::get(res, src)?;
        let destination = buffer::WgpuBuffer::get(res, dst)?;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hl-native-texture-to-buffer"),
            });

        // WGPU can write the protocol layout directly when its row pitch is representable. A one-row copy
        // has no row-to-row pitch and therefore omits `bytes_per_row`; taller copies require the WebGPU
        // 256-byte alignment. This is the presentation hot path for widths whose RGBA/BGRA row is already
        // aligned (for example 1728 * 4 = 6912): one copy command, one submission, no staging allocation.
        let direct = copy_rows == 1
            || u64::from(dst_stride).is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64);
        if direct {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &source.texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &destination.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: dst_offset,
                        bytes_per_row: (copy_rows > 1).then_some(dst_stride),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            self.gpu.queue.submit(Some(encoder.finish()));
            hl_log::hl_count!(hl_log::tag::PRESENT, "copy_texture_buffer_direct");
            hl_log::hl_add!(
                hl_log::tag::PRESENT,
                "copy_texture_buffer_direct_bytes",
                u64::from(row_bytes) * u64::from(copy_rows)
            );
            #[cfg(test)]
            {
                self.native_copy_submissions
                    .set(self.native_copy_submissions.get().saturating_add(1));
                self.direct_copy_submissions
                    .set(self.direct_copy_submissions.get().saturating_add(1));
            }
            return Ok(true);
        }

        let padded_row = row_bytes
            .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .ok_or(GpuError::OutOfBounds)?;
        let staging_size = u64::from(padded_row)
            .checked_mul(u64::from(copy_rows))
            .ok_or(GpuError::OutOfBounds)?;
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-native-texture-copy"),
            size: staging_size,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(copy_rows),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        for row in 0..copy_rows {
            encoder.copy_buffer_to_buffer(
                &staging,
                u64::from(row) * u64::from(padded_row),
                &destination.buffer,
                dst_offset + u64::from(row) * u64::from(dst_stride),
                u64::from(row_bytes),
            );
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        hl_log::hl_count!(hl_log::tag::PRESENT, "copy_texture_buffer_compact");
        hl_log::hl_add!(
            hl_log::tag::PRESENT,
            "copy_texture_buffer_compact_bytes",
            u64::from(row_bytes) * u64::from(copy_rows)
        );
        #[cfg(test)]
        self.native_copy_submissions
            .set(self.native_copy_submissions.get().saturating_add(1));
        Ok(true)
    }

    /// Convert any wgpu VALIDATION error raised while running `body` into a typed [`GpuError`], instead of
    /// letting it reach wgpu 24's default uncaptured-error handler (which PANICS). A hostile IR op that wgpu
    /// itself rejects — a draw whose vertex/index count overruns the bound buffer, a depth-tested pipeline
    /// drawn in a pass with NO depth attachment, a stencil op on a non-stencil target, an over-large compute
    /// dispatch — would otherwise abort the whole executor. wgpu validates a render/compute pass when the
    /// pass is ENDED (dropped) and again at submit, so the scope must wrap the ENTIRE pass, not just the
    /// submit. This pushes a validation scope, runs `body`, then ALWAYS pops it (balanced even on an early
    /// return from `body`, so the scope stack never leaks): a captured wgpu error becomes `Err` and the
    /// device is NOT lost (a following valid program still runs); `body`'s own typed error takes precedence.
    /// A well-formed program raises no error, so this is a transparent pass-through.
    fn with_validation_scope(&mut self, body: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let result = body(self);
        let captured = pollster::block_on(self.gpu.device.pop_error_scope());
        match (result, captured) {
            (Err(e), _) => Err(e),
            (Ok(()), Some(e)) => {
                // wgpu 24's `Display` for a validation error is the bare "Validation Error" — the ACTUAL
                // rule violated (which attachment/pipeline/format) lives in the Debug form and the
                // `std::error::Error::source()` chain, so surface both: `{e:?}` (Debug) plus every cause
                // walked off `source()`. This is what pins the offending pass without a wgpu_core log build.
                let mut chain = String::new();
                let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                while let Some(s) = src {
                    chain.push_str("\n  caused by: ");
                    chain.push_str(&s.to_string());
                    src = s.source();
                }
                hl_log::hl_error!(
                    tag::EXEC,
                    "wgpu rejected a pass at validation: {e}\n  debug: {e:?}{chain}"
                );
                Err(GpuError::Kernel(format!(
                    "wgpu: pass failed device validation: {e:?}{chain}"
                )))
            }
            (Ok(()), None) => Ok(()),
        }
    }
}

/// Find the encoder index of the pass-closing op matching the `Begin` at `start`, rejecting an unclosed
/// or nested pass (a validated command buffer never nests, but stay defensive).
fn find_end(ops: &[Enc], start: usize, close: Enc) -> Result<usize> {
    for (k, op) in ops.iter().enumerate().skip(start + 1) {
        if std::mem::discriminant(op) == std::mem::discriminant(&close) {
            return Ok(k);
        }
    }
    Err(GpuError::Invalid("command buffer ends inside an open pass"))
}

mod compute;
mod render;
mod transfer;

#[cfg(test)]
mod tests;
