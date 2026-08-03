//! `eglSwapBuffers` — the frame boundary + the one sink-touching op in the GL driver.
//!
//! Ported from `hl-shim-gl/src/egl.rs` (`eglSwapBuffers`). It builds the frame's `Cmd` stream from the
//! recorded draw-list ([`crate::service::frame::build_frame_ir`]), submits it through the
//! [`hl_gpu::CommandSink`], appends the `Present` that scans the rendered default target out to its
//! surface, then resets the per-frame draw state. This is the tested lowering surface: a driver test
//! drives it against a [`hl_gpu::RecordingSink`] and asserts the exact recorded command sequence.
//!
//! The submit is TRANSACTIONAL, matching the C shim: on a sink error the recorded draws are RETAINED
//! (the frame is not reset) so the caller can surface an `EGL_CONTEXT_LOST` and the frame is not
//! silently lost.

use crate::model::context::{FrameOp, GlContext, SurfaceKind};
use crate::service::frame::{self, FrameTarget};
use hl_gpu::{Cmd, CommandSink, Result};
use std::collections::HashSet;

/// Opaque description of the reads scheduled by [`schedule_transform_feedback_reads`]. The EGL transport
/// prepares I/O against a recording sink, then returns the actor's real observations later; keeping the
/// captures opaque prevents callers from applying the recording sink's zero-filled placeholders.
pub struct TransformFeedbackReads {
    captures: Vec<crate::model::context::TransformFeedbackReadback>,
}

pub fn schedule_transform_feedback_reads(
    ctx: &GlContext,
    sink: &mut dyn CommandSink,
) -> Result<TransformFeedbackReads> {
    let captures = ctx.local.transform_feedback_readbacks.clone();
    for capture in &captures {
        let _ = sink.read_buffer(hl_gpu::BufferId(capture.ir), 0, capture.len)?;
    }
    Ok(TransformFeedbackReads { captures })
}

pub fn apply_transform_feedback_reads(
    ctx: &mut GlContext,
    scheduled: TransformFeedbackReads,
    observations: Vec<Vec<u8>>,
) -> Result<()> {
    if observations.len() != scheduled.captures.len() {
        return Err(hl_gpu::GpuError::Invalid(
            "transform-feedback readback count mismatch",
        ));
    }
    for (capture, bytes) in scheduled.captures.iter().zip(&observations) {
        if bytes.len() != capture.len {
            return Err(hl_gpu::GpuError::Invalid(
                "short transform-feedback readback",
            ));
        }
    }
    if !ctx
        .local
        .transform_feedback_readbacks
        .starts_with(&scheduled.captures)
    {
        return Err(hl_gpu::GpuError::Invalid(
            "transform-feedback readback state changed",
        ));
    }
    for (capture, bytes) in scheduled.captures.iter().zip(observations) {
        ctx.buffers
            .set_sub_data(capture.buffer, capture.offset, &bytes);
    }
    ctx.local
        .transform_feedback_readbacks
        .drain(..scheduled.captures.len());
    for command in std::mem::take(&mut ctx.local.transform_feedback_cleanup) {
        ctx.queue_destroy(command);
    }
    Ok(())
}

/// `eglSwapBuffers()` — lower + submit + present the recorded frame, then reset per-frame state. Returns
/// `true` if a frame was presented, `false` if there was nothing (or nothing yet supported) to present
/// (a no-op swap — matching the shim's behaviour on an uncovered frame shape).
impl GlContext {
    fn deferred_texture_ids(&self) -> HashSet<u32> {
        self.local
            .recording
            .draws
            .iter()
            .flat_map(|draw| draw.textures.iter())
            .flat_map(|snapshot| [snapshot.sampled_ir, snapshot.fbo_ir])
            .chain(
                self.local
                    .recording
                    .blits
                    .iter()
                    .flat_map(|blit| [blit.read_ir, blit.draw_ir]),
            )
            .chain(
                self.local
                    .recording
                    .copy_tex
                    .iter()
                    .map(|copy| copy.read_ir),
            )
            .flatten()
            .collect()
    }

    /// Prune dead shared storage and submit its pending resource retirements.
    pub fn flush_retirements(&mut self, sink: &mut dyn CommandSink) -> Result<usize> {
        self.prune_shared_textures();
        if !self.has_pending_destroys() {
            return Ok(0);
        }
        // A partial window flush can transfer an ephemeral FBO target to a retained window draw. The
        // transfer queues the target's eventual retirement, but imported-image capture also reaches this
        // standalone cleanup boundary before the retained draw is swapped. Keep every texture named by a
        // deferred draw alive until that draw and its tail destroy submit atomically; otherwise cleanup
        // ACKs first and the following frame NACKs with `unknown/freed texture id`.
        //
        // Textures are deliberately the only resource kind pinned here. `TextureSnapshot::{sampled_ir,
        // fbo_ir}`, transferred `BlitOp::{read_ir,draw_ir}`, and `CopyTexOp::read_ir` are the host IR ids stored in deferred
        // work. Buffer snapshots own immutable bytes and re-resolve a fresh cached IR buffer after deletion;
        // programs retain GL names and re-resolve shader/pipeline caches; sampler snapshots retain
        // descriptors and re-resolve the persistent descriptor cache, while bind groups are frame-local. Surfaces, depth
        // targets, and fences are resolved from current context caches rather than captured as host ids. If
        // another host id is added to `DrawCall` or `FrameOp`, it must join this pin set.
        let pinned = self.deferred_texture_ids();
        let (ready, deferred): (Vec<_>, Vec<_>) =
            self.pending_destroys().iter().cloned().partition(
                |command| !matches!(command, Cmd::DestroyTexture(id) if pinned.contains(id)),
            );
        if ready.is_empty() {
            return Ok(0);
        }
        sink.submit(&ready)?;
        self.replace_pending_destroys(deferred);
        Ok(ready.len())
    }

    /// Keep ephemeral render targets alive when an accepted frame makes them authoritative for a sibling.
    pub fn retain_shared_targets(&self, frame: &mut frame::Frame) -> Vec<u32> {
        let retained = frame
            .targets
            .iter()
            .filter(|target| {
                target
                    .shared_storage
                    .is_some_and(|storage| self.textures.shared_residency(storage).is_some())
            })
            .map(|target| target.texture)
            .filter(|texture| {
                frame
                    .cmds
                    .iter()
                    .any(|command| matches!(command, Cmd::DestroyTexture(id) if id == texture))
            })
            .collect::<Vec<_>>();
        frame
            .cmds
            .retain(|command| !matches!(command, Cmd::DestroyTexture(id) if retained.contains(id)));
        retained
    }

    /// Commit render-target authority after a sink accepted the batch.
    ///
    /// Imported-image siblings then sample the accepted GPU target rather than an older CPU shadow. Callers
    /// must not invoke this before submission succeeds; rejected batches restore their pre-lowering state.
    pub fn accept_targets(&mut self, targets: &[FrameTarget]) {
        for target in targets {
            if let Some(storage) = target.shared_storage {
                if let (Some(revision), Some((_, residency))) = (
                    target.shared_revision,
                    self.textures.shared_residency(storage),
                ) {
                    self.promote_shared_texture(
                        storage,
                        revision,
                        target.width as u32,
                        target.height as u32,
                        target.texture,
                        residency,
                    );
                } else {
                    self.invalidate_shared_texture(storage);
                }
            }
            self.textures.mark_rendered(target.name, target.generation);
        }
    }

    pub fn swap_buffers(&mut self, sink: &mut dyn CommandSink) -> Result<bool> {
        let ctx = self;
        let frame_state = ctx.frame_state();
        let built = frame::Frame::build(ctx);
        let Some(mut f) = built else {
            ctx.restore_frame_state(frame_state);
            // Nothing to present. A frame that ONLY deleted resources (all draws no-ops) can still carry queued
            // persistent `Destroy*` — submit them standalone so the freed residency is reclaimed, then clear the
            // (possibly empty / unsupported) draw-list so the next frame starts clean.
            if ctx.has_pending_destroys() {
                let destroys = ctx.pending_destroys().to_vec();
                sink.submit(&destroys)?;
                ctx.clear_pending_destroys();
            }
            // …unless a `glReadPixels` already rendered this frame's default framebuffer. It consumed the
            // draw-list (so the frame executes once) but `glReadPixels` is not a frame boundary, so the swap
            // still has to post the render the readback left resident.
            let deferred = ctx.take_deferred_default_present();
            ctx.reset_frame();
            if deferred {
                if let Some((surface, texture)) = ctx.resident_default_draw_target() {
                    if ctx.local.present_token.is_some() && surface != 0 {
                        let serial = ctx
                            .local
                            .present_serial
                            .expect("native presentation carries a frame serial");
                        sink.submit(&[Cmd::Present {
                            surface,
                            texture,
                            serial,
                        }])?;
                    }
                    return Ok(true);
                }
            }
            return Ok(false);
        };

        // Flush the queued persistent `Destroy*` (glDelete* / content-change retirement) at the frame's tail —
        // AFTER its `Submit`s (the GPU work referencing the freed ids has run) and BEFORE the `Present`, matching
        // the per-draw ephemeral cleanup order. See `GlContext::pending_destroys`.
        f.cmds.extend_from_slice(ctx.pending_destroys());
        let retained_shared = ctx.retain_shared_targets(&mut f);

        // Native presentation is optional. The SHM compatibility path renders/readbacks without inventing
        // a host surface capability.
        let (surface, texture) = f.present;
        if ctx.local.present_token.is_some() && surface != 0 {
            f.cmds.push(Cmd::Present {
                surface,
                texture,
                serial: ctx
                    .local
                    .present_serial
                    .expect("native presentation carries a frame serial"),
            });
        }

        // TRANSACTIONAL: submit BEFORE resetting. On failure the draws (and the un-cleared pending destroys) are
        // retained and the error propagates; a rolled-back NACK re-emits the same destroys on the retry.
        if let Err(error) = sink.submit(&f.cmds) {
            crate::service::frame::refusal::report(ctx, &error, &f.cmds);
            ctx.restore_frame_state(frame_state);
            return Err(error);
        }

        ctx.clear_pending_destroys();
        ctx.accept_targets(&f.targets);
        ctx.own_shared_targets(&retained_shared);
        ctx.reset_frame();
        ctx.prune_shared_textures();
        Ok(true)
    }

    /// Execute pending work at `glFlush`/`glFinish` without consuming a window frame.
    ///
    /// A default framebuffer means different things depending on the EGL surface. Pbuffer and surfaceless
    /// contexts must execute framebuffer `0` at flush because they may never swap. A window context must retain
    /// framebuffer `0` until `eglSwapBuffers`; consuming it here leaves the compositor with an empty frame.
    /// Offscreen FBO work is submitted immediately in either case so Chrome's raster workers cannot accumulate
    /// an unbounded command list.
    pub fn flush(&mut self, sink: &mut dyn CommandSink) -> Result<bool> {
        let ctx = self;
        // BLITS are recorded work too. This asked only whether the DRAW list was empty, so a frame whose
        // only op was a `glBlitFramebuffer` returned here without ever consulting the builder — which is
        // willing (`Frame::build` guards on draws AND blits). A `glReadPixels` of the blit's destination
        // then read it as it was before the blit, and on an offscreen or pbuffer context there is no
        // `eglSwapBuffers` afterwards to execute it, so the copy simply never happened.
        if ctx.local.recording.draws.is_empty() && ctx.local.recording.blits.is_empty() {
            if ctx.has_pending_destroys() {
                let destroys = ctx.pending_destroys().to_vec();
                sink.submit(&destroys)?;
                ctx.clear_pending_destroys();
            }
            return Ok(false);
        }

        if ctx.local.surface_kind == SurfaceKind::Window {
            let draws = ctx.local.recording.draws.len();
            let original_draws = ctx.local.recording.draws.clone();
            let original_blits = ctx.local.recording.blits.clone();
            let original_copy_tex = ctx.local.recording.copy_tex.clone();
            let original_operations = ctx.local.recording.operations.clone();
            let (offscreen, window): (Vec<_>, Vec<_>) = original_draws
                .iter()
                .cloned()
                .partition(|draw| draw.fbo != 0);
            // Offscreen BLITS are flushed here too — the operation partition below routes exactly those
            // (`read_fbo != 0 && draw_fbo != 0`). Asking only about offscreen DRAWS dropped a frame whose
            // offscreen work was a blit, so this guard now asks the same question that partition does.
            let offscreen_blits = original_blits
                .iter()
                .any(|blit| blit.read_fbo != 0 && blit.draw_fbo != 0);
            let offscreen_copies = original_copy_tex.iter().any(|copy| copy.read_fbo != 0);
            if offscreen.is_empty() && !offscreen_blits && !offscreen_copies {
                return Ok(false);
            }

            ctx.local.recording.draws = offscreen;
            ctx.local.recording.operations = original_operations
                .iter()
                .filter(|operation| match operation {
                    FrameOp::Draw(draw) => draw.fbo != 0,
                    FrameOp::Blit(blit) => blit.read_fbo != 0 && blit.draw_fbo != 0,
                    FrameOp::CopyTex(copy) => copy.read_fbo != 0,
                    FrameOp::TexSubImage(upload) => upload.fbo != 0,
                })
                .cloned()
                .collect();
            ctx.local.recording.blits = ctx
                .local
                .recording
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    FrameOp::Blit(blit) => Some(*blit),
                    FrameOp::Draw(_) => None,
                    FrameOp::CopyTex(_) => None,
                    FrameOp::TexSubImage(_) => None,
                })
                .collect();
            ctx.local.recording.copy_tex = ctx.local.recording.operations.iter().filter_map(|operation| match operation {
                FrameOp::CopyTex(copy) => Some(*copy),
                _ => None,
            }).collect();
            let frame_state = ctx.frame_state();
            let built = frame::Frame::build(ctx);
            ctx.local.recording.draws = window;
            ctx.local.recording.operations = original_operations
                .iter()
                .filter(|operation| match operation {
                    FrameOp::Draw(draw) => draw.fbo == 0,
                    FrameOp::Blit(blit) => blit.read_fbo == 0 || blit.draw_fbo == 0,
                    FrameOp::CopyTex(copy) => copy.read_fbo == 0,
                    FrameOp::TexSubImage(_) => false,
                })
                .cloned()
                .collect();
            ctx.local.recording.blits = ctx
                .local
                .recording
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    FrameOp::Blit(blit) => Some(*blit),
                    FrameOp::Draw(_) => None,
                    FrameOp::CopyTex(_) => None,
                    FrameOp::TexSubImage(_) => None,
                })
                .collect();
            ctx.local.recording.copy_tex = ctx.local.recording.operations.iter().filter_map(|operation| match operation {
                FrameOp::CopyTex(copy) => Some(*copy),
                _ => None,
            }).collect();
            let Some(mut frame) = built else {
                ctx.restore_frame_state(frame_state);
                ctx.local.recording.draws = original_draws;
                ctx.local.recording.blits = original_blits;
                ctx.local.recording.copy_tex = original_copy_tex;
                ctx.local.recording.operations = original_operations;
                return Ok(false);
            };
            let retained_shared = ctx.retain_shared_targets(&mut frame);
            let frame_owned = frame
                .cmds
                .iter()
                .filter_map(|command| match command {
                    Cmd::DestroyTexture(id) => Some(*id),
                    _ => None,
                })
                .collect::<HashSet<_>>();
            let mut transferred = HashSet::new();
            let mut invalidated_shared = HashSet::new();
            for target in &frame.targets {
                for snapshot in ctx
                    .local
                    .recording
                    .draws
                    .iter_mut()
                    .flat_map(|draw| draw.textures.iter_mut())
                    .filter(|snapshot| {
                        snapshot.name == target.name && snapshot.generation == target.generation
                    })
                {
                    snapshot.fbo_ir = Some(target.texture);
                    if let Some(storage) = snapshot.texture.shared_storage() {
                        invalidated_shared.insert(storage);
                    }
                    snapshot.texture.gpu_authoritative = true;
                    if frame_owned.contains(&target.texture) {
                        transferred.insert(target.texture);
                    }
                }
                for operation in &mut ctx.local.recording.operations {
                    match operation {
                        FrameOp::Blit(blit) => {
                            if blit.read_target.is_some_and(|snapshot| snapshot.texture == target.name && snapshot.generation == target.generation) {
                                blit.read_ir = Some(target.texture);
                                if frame_owned.contains(&target.texture) { transferred.insert(target.texture); }
                            }
                            if blit.draw_target.is_some_and(|snapshot| snapshot.texture == target.name && snapshot.generation == target.generation) {
                                blit.draw_ir = Some(target.texture);
                                if frame_owned.contains(&target.texture) { transferred.insert(target.texture); }
                            }
                        }
                        FrameOp::CopyTex(copy) => {
                            if copy.read_target.is_some_and(|snapshot| snapshot.texture == target.name && snapshot.generation == target.generation) {
                                copy.read_ir = Some(target.texture);
                                if frame_owned.contains(&target.texture) { transferred.insert(target.texture); }
                            }
                        }
                        FrameOp::Draw(_) => {}
                        FrameOp::TexSubImage(_) => {}
                    }
                }
            }
            ctx.local.recording.blits = ctx
                .local
                .recording
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    FrameOp::Blit(blit) => Some(*blit),
                    FrameOp::Draw(_) => None,
                    FrameOp::CopyTex(_) => None,
                    FrameOp::TexSubImage(_) => None,
                })
                .collect();
            ctx.local.recording.copy_tex = ctx.local.recording.operations.iter().filter_map(|operation| match operation {
                FrameOp::CopyTex(copy) => Some(*copy),
                _ => None,
            }).collect();
            for storage in invalidated_shared {
                ctx.invalidate_shared_texture(storage);
            }
            frame.cmds.retain(
                |command| !matches!(command, Cmd::DestroyTexture(id) if transferred.contains(id)),
            );
            let pinned = ctx.deferred_texture_ids();
            let (ready, deferred): (Vec<_>, Vec<_>) =
                ctx.pending_destroys().iter().cloned().partition(
                    |command| !matches!(command, Cmd::DestroyTexture(id) if pinned.contains(id)),
                );
            frame.cmds.extend(ready);
            let commands = frame.cmds.len();
            if let Err(error) = sink.submit(&frame.cmds) {
                crate::service::frame::refusal::report(ctx, &error, &frame.cmds);
                ctx.restore_frame_state(frame_state);
                ctx.local.recording.draws = original_draws;
                ctx.local.recording.blits = original_blits;
                ctx.local.recording.operations = original_operations;
                return Err(error);
            }
            ctx.replace_pending_destroys(deferred);
            ctx.accept_targets(&frame.targets);
            ctx.own_shared_targets(&retained_shared);
            for texture in transferred {
                if !retained_shared.contains(&texture) {
                    ctx.queue_texture_destroy(texture);
                }
            }
            hl_log::hl_debug!(
                hl_log::tag::GL,
                "flush submitted cmds={} offscreen_of={} retained_window={}",
                commands,
                draws,
                ctx.local.recording.draws.len()
            );
            hl_log::hl_count!(hl_log::tag::GL, "offscreen_flushes");
            drop(original_draws);
            ctx.prune_shared_textures();
            return Ok(true);
        }

        let draws = ctx.local.recording.draws.len();
        let frame_state = ctx.frame_state();
        let built = frame::Frame::build(ctx);
        let Some(mut f) = built else {
            ctx.restore_frame_state(frame_state);
            if ctx.has_pending_destroys() {
                let destroys = ctx.pending_destroys().to_vec();
                sink.submit(&destroys)?;
                ctx.clear_pending_destroys();
            }
            // The recording is DISCARDED here, not retained: a frame that could not be built would
            // otherwise accumulate without bound. That is the right policy and the wrong silence — from
            // the application's side the draws simply never happened, and on a freshly minted target
            // (a resize retires the stale-sized one) the region they would have covered stays
            // zero-filled, which is transparency the user can see through. Name it, so the next
            // occurrence is attributable to the frame that failed to build rather than to the renderer.
            let (draws, blits) = ctx.recording_counts();
            if draws > 0 || blits > 0 {
                // ERROR, not warn, for the same reason as the missing-IR site: a warning is compiled out
                // of release, and this is the frame-level end of that chain. Naturally bounded — one line
                // per frame, and only when EVERYTHING recorded failed to lower — so if it repeats, the
                // repetition is itself the report.
                hl_log::hl_error!(
                    hl_log::tag::GL,
                    "discarding an unbuildable frame: {draws} draw(s) and {blits} blit(s) recorded and \
                     nothing lowered — their pixels will be missing from this frame"
                );
            }
            ctx.reset_frame();
            return Ok(false);
        };
        f.cmds.extend_from_slice(ctx.pending_destroys());
        let retained_shared = ctx.retain_shared_targets(&mut f);
        let cmds = f.cmds.len();
        if let Err(error) = sink.submit(&f.cmds) {
            crate::service::frame::refusal::report(ctx, &error, &f.cmds);
            ctx.restore_frame_state(frame_state);
            return Err(error);
        }
        ctx.clear_pending_destroys();
        ctx.accept_targets(&f.targets);
        ctx.own_shared_targets(&retained_shared);
        ctx.reset_frame();
        ctx.prune_shared_textures();
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "flush submitted cmds={} offscreen_draws={}",
            cmds,
            draws
        );
        hl_log::hl_count!(hl_log::tag::GL, "flushes");
        Ok(true)
    }
}

pub const SWAP_BUFFERS: fn(&mut GlContext, &mut dyn CommandSink) -> Result<bool> =
    GlContext::swap_buffers;
pub const FLUSH: fn(&mut GlContext, &mut dyn CommandSink) -> Result<bool> = GlContext::flush;
pub use FLUSH as flush;
pub use SWAP_BUFFERS as swap_buffers;

#[cfg(test)]
mod transform_feedback_read_tests {
    use super::*;
    use crate::model::context::TransformFeedbackReadback;

    #[test]
    fn prepared_placeholder_is_not_applied_and_actor_bytes_are() {
        let mut ctx = GlContext::new();
        let buffer = ctx.buffers.gen();
        ctx.buffers.set_data(
            buffer,
            crate::model::glconst::GL_TRANSFORM_FEEDBACK_BUFFER,
            &[0x55; 8],
            0,
        );
        ctx.local
            .transform_feedback_readbacks
            .push(TransformFeedbackReadback {
                ir: 7,
                buffer,
                offset: 2,
                len: 4,
            });
        let mut prepared = hl_gpu::RecordingSink::with_full_caps();
        let scheduled = schedule_transform_feedback_reads(&ctx, &mut prepared).unwrap();
        assert_eq!(ctx.buffers.get(buffer).unwrap().data.as_slice(), &[0x55; 8]);

        apply_transform_feedback_reads(&mut ctx, scheduled, vec![vec![1, 2, 3, 4]]).unwrap();
        assert_eq!(
            ctx.buffers.get(buffer).unwrap().data.as_slice(),
            &[0x55, 0x55, 1, 2, 3, 4, 0x55, 0x55]
        );
        assert!(ctx.local.transform_feedback_readbacks.is_empty());
    }

    #[test]
    fn short_actor_read_keeps_pending_capture_and_mirror() {
        let mut ctx = GlContext::new();
        let buffer = ctx.buffers.gen();
        ctx.buffers.set_data(
            buffer,
            crate::model::glconst::GL_TRANSFORM_FEEDBACK_BUFFER,
            &[9; 4],
            0,
        );
        ctx.local
            .transform_feedback_readbacks
            .push(TransformFeedbackReadback {
                ir: 3,
                buffer,
                offset: 0,
                len: 4,
            });
        let mut prepared = hl_gpu::RecordingSink::with_full_caps();
        let scheduled = schedule_transform_feedback_reads(&ctx, &mut prepared).unwrap();

        assert!(apply_transform_feedback_reads(&mut ctx, scheduled, vec![vec![1, 2]]).is_err());
        assert_eq!(ctx.buffers.get(buffer).unwrap().data.as_slice(), &[9; 4]);
        assert_eq!(ctx.local.transform_feedback_readbacks.len(), 1);
    }
}
