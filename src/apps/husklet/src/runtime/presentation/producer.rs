//! Native GPU presentation publication after successful command execution.

use hl_compositor::adapter::smithay::{
    NativeFrame, NativeFrameOutcome, NativeFramePublishFailure, NativeFrameReceipt,
    NativeFrameSender,
};
use hl_gpu::protocol::model::descriptor::SurfaceDesc;
use hl_gpu::{Cmd, Presentation, Session};

use crate::runtime::gpu::executor::Executor;

pub(crate) struct Producer {
    publisher: NativeFrameSender,
    receipts: Vec<NativeFrameReceipt>,
    /// Presentations that cannot be published because their texture has no IOSurface. Latched per
    /// (surface, texture) so the diagnostic carries an occurrence count and a window rather than
    /// repeating at display rate.
    unpublishable: hl_compositor::diagnostic::Tally<(u32, u32)>,
}

impl Producer {
    pub(crate) fn new(publisher: NativeFrameSender) -> Self {
        Self {
            publisher,
            receipts: Vec::new(),
            unpublishable: hl_compositor::diagnostic::Tally::default(),
        }
    }

    pub(crate) fn publish(
        &mut self,
        session: &Session,
        executor: &Executor,
        presentations: Vec<Presentation>,
    ) {
        self.receipts
            .retain(|receipt| match receipt.try_complete() {
                Ok(Some(completion)) => {
                    let level = match completion.outcome {
                        NativeFrameOutcome::Displayed | NativeFrameOutcome::Discarded => {
                            hl_log::Level::Debug
                        }
                        _ => hl_log::Level::Warn,
                    };
                    hl_log::hl_log!(
                        hl_log::tag::PRESENT,
                        level,
                        "native presentation completed token={} serial={} outcome={:?}",
                        completion.token,
                        completion.serial,
                        completion.outcome
                    );
                    false
                }
                Ok(None) => true,
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::PRESENT,
                        "native presentation completion channel failed error={error}"
                    );
                    false
                }
            });
        for presentation in presentations {
            let diagnostics = hl_log::Logging::global().enabled(
                hl_log::Tags::from(hl_log::tag::PRESENT),
                hl_log::Level::Debug,
            );
            let total_started = diagnostics.then(std::time::Instant::now);
            let extraction_started = diagnostics.then(std::time::Instant::now);
            let image = match executor.image(&session.resources, presentation) {
                Ok(Some(image)) => image,
                Ok(None) => {
                    // NOT a silent `continue`. This arm means the presented texture has no IOSurface
                    // backing, so no native frame can be published for it — and the compositor is at
                    // that moment holding a commit deferred on exactly this `(token, serial)`, which
                    // will now never arrive. Nothing downstream can report it: the compositor cannot
                    // distinguish a commit parked forever from one never made, and the guest sees
                    // `VK_SUCCESS`. Skipping quietly here is what made a completely invisible window
                    // an investigation instead of a lookup.
                    //
                    // Error level so it survives a release build, latched per texture so a surface
                    // presenting at display rate contributes a line rather than a flood.
                    if let Some(seen) = self
                        .unpublishable
                        .record((presentation.surface.0, presentation.texture.0))
                    {
                        hl_log::hl_error!(
                            hl_log::tag::PRESENT,
                            "native presentation has no IOSurface surface={} texture={} token={} \
                             serial={} count={} over_ms={} — the texture was not created with the \
                             PRESENT usage, so nothing is published and the compositor's commit for \
                             this token/serial can never complete",
                            presentation.surface.0,
                            presentation.texture.0,
                            presentation.token.get(),
                            presentation.serial.get(),
                            seen.count,
                            seen.since.as_millis()
                        );
                    }
                    continue;
                }
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::PRESENT,
                        "native presentation lookup failed surface={} texture={} error={error}",
                        presentation.surface.0,
                        presentation.texture.0
                    );
                    continue;
                }
            };
            let extraction_us = extraction_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default();
            let frame_started = diagnostics.then(std::time::Instant::now);
            let completion = image.completion();
            let device = completion.device_key();
            let frame = match NativeFrame::new(
                presentation.token.get(),
                presentation.serial.get(),
                image.surface().clone(),
            ) {
                Ok(frame) => frame.with_device_readiness(device, move || completion.is_ready()),
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::PRESENT,
                        "native presentation identity rejected surface={} error={error:?}",
                        presentation.surface.0
                    );
                    continue;
                }
            };
            let frame_us = frame_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default();
            if std::env::var_os("HL_GL_CAPTURE_PIXELS").is_some() {
                eprintln!(
                    "capture ownership boundary=gpu_publish token={} serial={} iosurface={} texture={}",
                    presentation.token.get(),
                    presentation.serial.get(),
                    image.surface().id(),
                    presentation.texture.0
                );
            }
            let publication_started = diagnostics.then(std::time::Instant::now);
            let (width, height, stride) = image.surface().dimensions();
            match self.publisher.publish(frame) {
                Ok(receipt) => {
                    let publication_us = publication_started
                        .map(|started| started.elapsed().as_micros())
                        .unwrap_or_default();
                    hl_log::hl_debug!(
                        hl_log::tag::PRESENT,
                        "native presentation published surface={} texture={} iosurface={} width={} height={} stride={} token={} serial={} extraction_us={} frame_us={} publication_us={} total_us={}",
                        presentation.surface.0,
                        presentation.texture.0,
                        image.surface().id(),
                        width,
                        height,
                        stride,
                        presentation.token.get(),
                        presentation.serial.get(),
                        extraction_us,
                        frame_us,
                        publication_us,
                        total_started
                            .map(|started| started.elapsed().as_micros())
                            .unwrap_or_default()
                    );
                    self.receipts.push(receipt);
                }
                Err(error) => {
                    let reason = match error.reason {
                        NativeFramePublishFailure::Closed => "closed",
                    };
                    drop(error.frame);
                    hl_log::hl_log!(
                        hl_log::tag::PRESENT,
                        hl_log::Level::Warn,
                        "native presentation dropped surface={} token={} serial={} reason={reason}",
                        presentation.surface.0,
                        presentation.token.get(),
                        presentation.serial.get()
                    );
                }
            }
        }
    }

    /// Capture native identities before execution mutates or rolls back session resources.
    pub(crate) fn reservations(&self, session: &Session, batch: &[Cmd]) -> Vec<(u64, u64)> {
        let mut created = std::collections::HashMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut reservations = Vec::new();
        for command in batch {
            match command {
                Cmd::CreateSurface(id, descriptor) => {
                    created.insert(*id, descriptor.token);
                }
                Cmd::DestroySurface(id) => {
                    created.remove(id);
                }
                Cmd::Present {
                    surface, serial, ..
                } => {
                    let token = created.get(surface).copied().or_else(|| {
                        session
                            .resources
                            .surfaces
                            .get(*surface)
                            .ok()?
                            .downcast_ref::<SurfaceDesc>()
                            .map(|descriptor| descriptor.token)
                    });
                    if let Some(token) = token {
                        let identity = (token.get(), serial.get());
                        if seen.insert(identity) {
                            reservations.push(identity);
                        }
                    }
                }
                _ => {}
            }
        }
        reservations
    }

    /// Settle identities exposed by the client before a GPU batch that ultimately failed.
    pub(crate) fn cancel(&self, reservations: &[(u64, u64)]) {
        for &(token, serial) in reservations {
            if let Err(error) = self.publisher.cancel(token, serial) {
                hl_log::hl_warn!(
                    hl_log::tag::PRESENT,
                    "native presentation cancellation failed token={token} serial={serial} error={error:?}"
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn completion(
        &self,
        index: usize,
    ) -> Option<hl_compositor::adapter::smithay::NativeFrameCompletion> {
        self.receipts.get(index)?.try_complete().ok().flatten()
    }
}
