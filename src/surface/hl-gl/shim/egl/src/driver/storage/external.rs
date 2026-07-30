pub(super) fn submit_external(
    bindings: &std::collections::HashMap<u32, crate::state::ImportedImage>,
    publications: &[(
        hl_gl::service::frame::FrameTarget,
        hl_gpu::protocol::model::descriptor::FrameSerial,
    )],
    submit: impl FnOnce() -> hl_gpu::Result<()>,
) -> hl_gpu::Result<()> {
    // Validate the complete set before making any identity externally visible.
    for &(target, _) in publications {
        let binding = bindings.get(&target.name).ok_or_else(|| {
            hl_gpu::GpuError::Decode("external image binding disappeared before publication".into())
        })?;
        if binding.generation != target.generation {
            return Err(hl_gpu::GpuError::Decode(
                "external image generation changed before publication".into(),
            ));
        }
    }
    for &(target, serial) in publications {
        bindings[&target.name]
            .image
            .publish(serial.get())
            .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
    }
    submit()
}

pub(super) fn flush_imported_images(
    group: &mut crate::state::GroupData,
    sink: &mut dyn hl_gpu::CommandSink,
    max_buffer_bytes: u64,
) -> hl_gpu::Result<Option<usize>> {
    if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
        let framebuffer = group.gl.bound_framebuffer();
        let attachment = group.gl.framebuffer_color_attachment(framebuffer);
        let generation = group.gl.textures.get(attachment).map(|texture| texture.gen);
        eprintln!(
            "capture ownership boundary=egl_image_flush framebuffer={} attachment={} generation={generation:?} imported={} draws={} blits={}",
            framebuffer,
            attachment,
            group.images.contains_key(&attachment),
            group.gl.recording_counts().0,
            group.gl.recording_counts().1
        );
    }
    let uploaded = group.flush_dirty_images(&std::collections::HashSet::new())?;
    if group.gl.recording_counts().0 == 0 {
        let retired = swap::flush(&mut group.gl, sink)?;
        return Ok(Some(uploaded.max(usize::from(retired))));
    }
    let imported = group.images.keys().copied().collect::<Vec<_>>();
    let frame_state = group.gl.frame_state();
    let Some((mut frame, captures)) =
        hl_gl::service::frame::Frame::build_captured(&mut group.gl, imported, max_buffer_bytes)?
    else {
        return Ok(None);
    };
    let diagnostic_captures = if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
        frame.capture_external_targets(&mut group.gl, max_buffer_bytes)?
    } else {
        Vec::new()
    };
    frame.cmds.extend_from_slice(group.gl.pending_destroys());
    let retained_shared = group.gl.retain_shared_targets(&mut frame);
    let publications = match frame.append_external_presents(|| group.gl.alloc_frame_serial()) {
        Ok(publications) => publications,
        Err(error) => {
            group.gl.restore_frame_state(frame_state);
            return Err(error);
        }
    };

    // Publish the identity before entering the synchronous GPU submission. The Wayland client owns the
    // dma-buf and may commit it from another thread while the host executes this batch. Its commit must see
    // the exact token+serial and defer until the matching native frame arrives; publishing afterward loses
    // that association race and leaves the completed IOSurface orphaned.
    //
    // A NACK carries the same Present commands to the host, which terminally cancels these identities.
    // Therefore a commit racing either outcome settles: success joins the native frame, failure discards the
    // deferred commit instead of waiting forever.
    if let Err(error) = submit_external(&group.images, &publications, || sink.submit(&frame.cmds)) {
        group.gl.restore_frame_state(frame_state);
        return Err(error);
    }
    // Submission is the execution boundary. A later readback or host-plane write failure must not replay
    // already-executed GL commands on the next flush.
    group.gl.clear_pending_destroys();
    group.gl.accept_targets(&frame.targets);
    group.gl.own_shared_targets(&retained_shared);
    group.gl.reset_frame();
    let cleanup_buffers = captures
        .iter()
        .map(|capture| capture.buffer)
        .chain(diagnostic_captures.iter().map(|capture| capture.buffer))
        .collect::<std::collections::HashSet<_>>();
    for buffer in &cleanup_buffers {
        group.gl.queue_buffer_destroy(*buffer);
    }
    for capture in &diagnostic_captures {
        let readback_len = capture
            .offset
            .checked_add(
                u64::from(capture.bytes_per_row)
                    .checked_mul(capture.target.height.max(1) as u64)
                    .ok_or_else(|| {
                        hl_gpu::GpuError::Decode(
                            "external diagnostic readback size overflow".into(),
                        )
                    })?,
            )
            .and_then(|len| usize::try_from(len).ok())
            .ok_or_else(|| {
                hl_gpu::GpuError::Decode("external diagnostic readback size overflow".into())
            })?;
        let readback = sink.read_buffer(BufferId(capture.buffer), 0, readback_len)?;
        let bgra = compact_capture(&readback, capture)?;
        let min = bgra.iter().copied().min().unwrap_or_default();
        let max = bgra.iter().copied().max().unwrap_or_default();
        let nonzero = bgra.iter().filter(|byte| **byte != 0).count();
        let serial = capture.target.token.and_then(|token| {
            publications
                .iter()
                .find(|(target, _)| target.token == Some(token))
                .map(|(_, serial)| serial.get())
        });
        eprintln!(
            "capture pixel boundary=egl_external_gpu name={} generation={} token={:?} serial={serial:?} texture={} size={}x{} bytes={} min={} max={} nonzero={}",
            capture.target.name,
            capture.target.generation,
            capture.target.token.map(|token| token.get()),
            capture.target.texture,
            capture.target.width,
            capture.target.height,
            bgra.len(),
            min,
            max,
            nonzero
        );
    }

    let mut buffers = Vec::<(u32, usize)>::new();
    for capture in &captures {
        let end = capture
            .offset
            .checked_add(
                u64::from(capture.bytes_per_row)
                    .checked_mul(capture.target.height.max(1) as u64)
                    .ok_or_else(|| {
                        hl_gpu::GpuError::Decode("imported image readback size overflow".into())
                    })?,
            )
            .and_then(|end| usize::try_from(end).ok())
            .ok_or_else(|| {
                hl_gpu::GpuError::Decode("imported image readback size overflow".into())
            })?;
        if let Some((_, len)) = buffers
            .iter_mut()
            .find(|(buffer, _)| *buffer == capture.buffer)
        {
            *len = (*len).max(end);
        } else {
            buffers.push((capture.buffer, end));
        }
    }
    let len = buffers.iter().try_fold(0usize, |total, (_, len)| {
        total
            .checked_add(*len)
            .ok_or_else(|| hl_gpu::GpuError::Decode("imported image readback size overflow".into()))
    })?;
    hl_log::hl_debug!(
        hl_log::tag::PRESENT,
        "readback reason=egl_image_flush targets={} bytes={len}",
        captures.len()
    );
    hl_log::hl_add!(hl_log::tag::PRESENT, "readback_egl_image_flush", 1);
    hl_log::hl_add!(
        hl_log::tag::PRESENT,
        "readback_egl_image_flush_targets",
        captures.len() as u64
    );
    hl_log::hl_add!(
        hl_log::tag::PRESENT,
        "readback_egl_image_flush_bytes",
        len as u64
    );

    let mut first_error = None;
    for (buffer, len) in &buffers {
        let readback = match sink.read_buffer(BufferId(*buffer), 0, *len) {
            Ok(readback) => readback,
            Err(error) => {
                first_error.get_or_insert(error);
                continue;
            }
        };
        for capture in captures.iter().filter(|capture| capture.buffer == *buffer) {
            let pixels = compact_capture(&readback, capture);
            let result = pixels.and_then(|bgra| {
                let image = group
                    .images
                    .get(&capture.target.name)
                    .cloned()
                    .ok_or_else(|| {
                        hl_gpu::GpuError::Decode(
                            "imported image binding disappeared during flush".into(),
                        )
                    })?;
                if image.image.width != capture.target.width as u32
                    || image.image.height != capture.target.height as u32
                {
                    return Err(hl_gpu::GpuError::Decode(
                        "imported image dimensions changed during flush".into(),
                    ));
                }
                if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
                    let min = bgra.iter().copied().min().unwrap_or_default();
                    let max = bgra.iter().copied().max().unwrap_or_default();
                    let nonzero = bgra.iter().filter(|byte| **byte != 0).count();
                    eprintln!(
                        "capture pixel boundary=egl_image_gpu name={} generation={} token={:?} size={}x{} bytes={} min={} max={} nonzero={}",
                        capture.target.name,
                        capture.target.generation,
                        capture.target.token.map(|token| token.get()),
                        capture.target.width,
                        capture.target.height,
                        bgra.len(),
                        min,
                        max,
                        nonzero
                    );
                }
                image
                    .image
                    .write_native_bgra(&bgra)
                    .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
                if let Some(shared) = &image.shared {
                    shared.store(Arc::new(bgra.clone()));
                }
                if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
                    let written = image
                        .image
                        .native_bgra()
                        .map_err(|error| hl_gpu::GpuError::Decode(error.to_string()))?;
                    let min = written.iter().copied().min().unwrap_or_default();
                    let max = written.iter().copied().max().unwrap_or_default();
                    let nonzero = written.iter().filter(|byte| **byte != 0).count();
                    eprintln!(
                        "capture pixel boundary=egl_image_host name={} generation={} token={:?} size={}x{} bytes={} min={} max={} nonzero={}",
                        capture.target.name,
                        capture.target.generation,
                        capture.target.token.map(|token| token.get()),
                        capture.target.width,
                        capture.target.height,
                        written.len(),
                        min,
                        max,
                        nonzero
                    );
                }
                Ok(())
            });
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
    }

    if let Err(error) = group.gl.flush_retirements(sink) {
        first_error.get_or_insert(error);
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(Some(
            captures.len().max(usize::from(!frame.targets.is_empty())),
        )),
    }
}

pub(super) fn compact_capture(
    readback: &[u8],
    capture: &hl_gl::service::frame::FrameCapture,
) -> hl_gpu::Result<Vec<u8>> {
    let start = usize::try_from(capture.offset).map_err(|_| {
        hl_gpu::GpuError::Decode("imported image readback offset is invalid".into())
    })?;
    let pitch = capture.bytes_per_row as usize;
    let row = capture.target.width.max(1) as usize * 4;
    let height = capture.target.height.max(1) as usize;
    let end = start
        .checked_add(pitch.checked_mul(height).ok_or_else(|| {
            hl_gpu::GpuError::Decode("imported image readback size overflow".into())
        })?)
        .ok_or_else(|| hl_gpu::GpuError::Decode("imported image readback range overflow".into()))?;
    let padded = readback.get(start..end).ok_or_else(|| {
        hl_gpu::GpuError::Decode("imported image readback range is invalid".into())
    })?;
    let mut compact = Vec::with_capacity(capture.len);
    for source in padded.chunks_exact(pitch).take(height) {
        compact.extend_from_slice(source.get(..row).ok_or_else(|| {
            hl_gpu::GpuError::Decode("imported image row pitch is invalid".into())
        })?);
    }
    if compact.len() != capture.len {
        return Err(hl_gpu::GpuError::Decode(
            "imported image compacted length is invalid".into(),
        ));
    }
    Ok(compact)
}
use super::*;
