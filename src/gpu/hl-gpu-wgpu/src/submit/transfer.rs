use super::*;

impl WgpuExecutor {
    /// Fill `[offset, offset+size)` of buffer `id` with the repeating little-endian pattern of `value`
    /// (device memset). Read-modify-write over the 4-aligned window preserves neighbour bytes and matches
    /// the oracle's tiling (buffer byte `offset+i` takes pattern byte `i % 4`).
    pub(super) fn fill_buffer(
        &self,
        res: &SessionResources,
        id: u32,
        offset: u64,
        size: u64,
        value: u32,
    ) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        // The fill window `[offset, offset+size)` must lie inside the buffer. `offset + size` is computed
        // below (twice, including a `div_ceil`); a hostile `offset`/`size` near `u64::MAX` overflows that
        // arithmetic (a debug panic) and, unchecked, would also read/write past the allocation. The runtime
        // does not range-check `FillBuffer`, so guard it into a typed `OutOfBounds` (matching `read_bytes`/
        // `write_bytes`).
        let b = buffer::WgpuBuffer::get(res, id)?;
        let end = offset
            .checked_add(size)
            .filter(|e| *e <= b.size)
            .ok_or(GpuError::OutOfBounds)?;
        let pat = value.to_le_bytes();
        let astart = offset & !3;
        let aend = end.div_ceil(4) * 4;
        let mut window = self.read_bytes(res, id, astart, (aend - astart) as usize)?;
        for p in offset..end {
            window[(p - astart) as usize] = pat[((p - offset) % 4) as usize];
        }
        self.write_bytes(res, id, astart, &window)
    }

    /// Exact (no-scaling) texture→texture copy: move `extent` texels from `src`'s `src_origin` to `dst`'s
    /// `dst_origin`, CPU-mediated through the tight readback plane + region upload (mirrors the CPU oracle's
    /// `copy_texture_to_texture`). Only the base subresource (mip 0 / layer 0 / whole color aspect) of a 2D
    /// color texture is supported; anything else, a format-size mismatch, or an out-of-range region is a
    /// clean typed error rather than a panic (the runtime does not range-check this op).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_texture_to_texture(
        &mut self,
        res: &SessionResources,
        src: u32,
        src_sub: &TextureSubresource,
        src_origin: &Origin3d,
        dst: u32,
        dst_sub: &TextureSubresource,
        dst_origin: &Origin3d,
        extent: &Extent3d,
    ) -> Result<()> {
        // Copy-compatibility gate. wgpu's raw texel copy — and the CPU-mediated byte copy below, which
        // reinterprets the source bytes verbatim in the destination — is only correct when the two formats
        // share an IDENTICAL byte layout: the same wgpu format ignoring an sRGB suffix
        // (`Rgba8Unorm` ↔ `Rgba8UnormSrgb` differ only in transfer interpretation, not bytes). A copy across
        // a DIFFERENT layout — a channel-order swap (`Rgba8` ↔ `Bgra8`), a different texel size, or an sRGB
        // vs linear reinterpretation — is what GL permits through a CONVERTING copy (`glBlitFramebuffer` /
        // `glCopyTexSubImage2D` / `glCopyImageSubData`): it re-samples the source and RE-ENCODES into the
        // destination format. A byte copy would silently corrupt (wrong channel order) or, on a size
        // mismatch, be rejected. So for a format mismatch we route through a CONVERTING BLIT — sample the
        // source region, render it into the destination region, and let wgpu channel-swap / re-encode on
        // write — a 1:1 (`src_extent == dst_extent == extent`) blit with NEAREST filter, i.e. an exact
        // texel remap with no resampling. Copy-COMPATIBLE formats keep the fast raw byte copy below, so
        // there is no behaviour or perf change for existing apps.
        let (src_wfmt, dst_wfmt) = {
            let s = texture::WgpuTexture::get(res, src)?;
            let d = texture::WgpuTexture::get(res, dst)?;
            (
                Format::from(s.format).native(),
                Format::from(d.format).native(),
            )
        };
        if src_wfmt.remove_srgb_suffix() != dst_wfmt.remove_srgb_suffix() {
            hl_log::hl_debug!(
                tag::EXEC,
                "t2t converting-copy: src={src_wfmt:?} dst={dst_wfmt:?} (incompatible formats → blit)"
            );
            hl_log::hl_count!(tag::EXEC, "t2t_converting_copy");
            // A 1:1 blit: same source and destination extent (no scaling), nearest sampling (exact texel
            // mapping). `blit_texture` performs the same base-subresource / bounds / 2D-only validation the
            // fast path does, so a bad subresource or out-of-range region is the same typed error.
            return self.blit_texture(
                res,
                src,
                src_sub,
                src_origin,
                extent,
                dst,
                dst_sub,
                dst_origin,
                extent,
                Filter::Nearest,
            );
        }
        for sub in [src_sub, dst_sub] {
            if sub.mip != 0 || sub.layer != 0 || sub.aspect != TextureAspect::All {
                return Err(GpuError::Unsupported(
                    "wgpu: non-base subresource texture copy",
                ));
            }
        }
        if src_origin.z != 0 || dst_origin.z != 0 || extent.depth > 1 {
            return Err(GpuError::Unsupported(
                "wgpu: 3D/layer texture-to-texture copy",
            ));
        }
        let (sw, sh, s_bpt) = {
            let t = texture::WgpuTexture::get(res, src)?;
            (
                t.width,
                t.height,
                Format::from(t.format).texel_bytes()? as u32,
            )
        };
        let (dw, dh, d_bpt) = {
            let t = texture::WgpuTexture::get(res, dst)?;
            (
                t.width,
                t.height,
                Format::from(t.format).texel_bytes()? as u32,
            )
        };
        // Copy-compatible formats (checked above) always share a texel size; keep the guard as a defensive
        // invariant so a future format whose sRGB base collides but whose byte size differs cannot slip
        // through as a silent mis-copy.
        if s_bpt != d_bpt {
            return Err(GpuError::Invalid(
                "wgpu: texture-to-texture copy between incompatible formats",
            ));
        }
        let (ew, eh) = (extent.width, extent.height);
        // Range guards (wrapping-safe): the source region must lie in `src`, the dest region in `dst`.
        let ok = |x: u32, y: u32, w: u32, h: u32, tw: u32, th: u32| {
            x.checked_add(w).is_some_and(|e| e <= tw) && y.checked_add(h).is_some_and(|e| e <= th)
        };
        if !ok(src_origin.x, src_origin.y, ew, eh, sw, sh)
            || !ok(dst_origin.x, dst_origin.y, ew, eh, dw, dh)
        {
            return Err(GpuError::OutOfBounds);
        }
        let bpt = s_bpt as usize;
        let sw = sw as usize;
        let plane = self.read_texture_tight(res, src)?;
        let (sx, sy) = (src_origin.x as usize, src_origin.y as usize);
        let row = ew as usize * bpt;
        let mut block = Vec::with_capacity(row * eh as usize);
        for r in 0..eh as usize {
            let start = ((sy + r) * sw + sx) * bpt;
            block.extend_from_slice(&plane[start..start + row]);
        }
        self.write_region(
            res,
            dst,
            dst_origin.x,
            dst_origin.y,
            0,
            ew,
            eh,
            1,
            0,
            &block,
        )
    }

    /// Multisample resolve: average the samples of multisampled `src` into single-sample `dst`. wgpu
    /// exposes resolve only as a render-pass `resolve_target`, so this begins a zero-draw pass that LOADs
    /// `src` as its color attachment (the samples a prior pass rendered + stored) and names `dst` as the
    /// resolve target; wgpu resolves at pass end. Only the base subresource of a whole 2D color texture is
    /// supported (wgpu resolves the WHOLE attachment, so a sub-rect origin/extent that is not the full
    /// matching texture is a clean typed error, not a silent partial resolve). `src` must be multisampled
    /// and `dst` single-sampled of the same size + format — anything else is rejected rather than guessed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_texture(
        &mut self,
        res: &SessionResources,
        src: u32,
        src_sub: &TextureSubresource,
        src_origin: &Origin3d,
        dst: u32,
        dst_sub: &TextureSubresource,
        dst_origin: &Origin3d,
        extent: &Extent3d,
    ) -> Result<()> {
        for sub in [src_sub, dst_sub] {
            if sub.mip != 0 || sub.layer != 0 || sub.aspect != TextureAspect::All {
                return Err(GpuError::Unsupported(
                    "wgpu: non-base subresource multisample resolve",
                ));
            }
        }
        let (src_view, src_samples, sw, sh, sfmt) = {
            let t = texture::WgpuTexture::get(res, src)?;
            (t.view.clone(), t.sample_count, t.width, t.height, t.format)
        };
        let (dst_view, dst_samples, dw, dh, dfmt) = {
            let t = texture::WgpuTexture::get(res, dst)?;
            (t.view.clone(), t.sample_count, t.width, t.height, t.format)
        };
        if src_samples <= 1 {
            return Err(GpuError::Invalid(
                "wgpu: resolve source is not multisampled",
            ));
        }
        if dst_samples != 1 {
            return Err(GpuError::Invalid(
                "wgpu: resolve destination must be single-sampled",
            ));
        }
        if sfmt != dfmt {
            return Err(GpuError::Invalid(
                "wgpu: resolve between incompatible formats",
            ));
        }
        // A render-pass resolve resolves the ENTIRE attachment, so only a whole-texture resolve (origin 0,
        // extent == both textures' matching size) is faithful; a sub-region resolve would silently resolve
        // more than asked, so it is rejected.
        if src_origin.x != 0
            || src_origin.y != 0
            || src_origin.z != 0
            || dst_origin.x != 0
            || dst_origin.y != 0
            || dst_origin.z != 0
            || extent.width != sw
            || extent.height != sh
            || extent.depth > 1
            || sw != dw
            || sh != dh
        {
            return Err(GpuError::Unsupported(
                "wgpu: sub-region multisample resolve",
            ));
        }
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-resolve-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &src_view,
                    // The single-sample destination receives the averaged (resolved) samples at pass end.
                    resolve_target: Some(&dst_view),
                    ops: wgpu::Operations {
                        // LOAD the samples a prior pass rendered + stored into this MSAA target.
                        load: wgpu::LoadOp::Load,
                        // The MSAA samples themselves need not be kept once resolved.
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.gpu.queue.submit(Some(enc.finish()));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }
}
