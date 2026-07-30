//! [`PngPresenter`]: the headless [`Presenter`] the Smithay adapter drives.
//!
//! The neutral [`Presenter`] port carries only geometry — no pixels — so this presenter keeps a small
//! side store of the actual client pixels the adapter deposits at commit time (keyed by [`SurfaceId`]).
//! When the scene composes a frame and calls [`Presenter::present`] for the base layer, the presenter
//! looks up that surface's deposited pixels, records a [`CapturedFrame`], and (optionally) writes a real
//! `.png` to disk. A test reads the captured frames back through the shared `captures` handle and asserts
//! the client's pixels made it all the way through wl → scene → present, fully headless.

#[cfg(feature = "macos-surface")]
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::scene::model::{BufferTransform, Rect, SurfaceId};
use crate::scene::port::{
    Clipboard, HostEvents, HostSurface, PresentFrame, PresentationFeedback, Presenter, Windows,
};

mod buffer;
mod headless;
mod png;

pub use buffer::StoredBuffer;
pub use headless::{CapturedFrame, Observations, PngPresenter};
pub use png::{encode_png, write_png};

/// Pixel-delivery extension for presenters that receive committed Smithay buffers out of band.
pub trait SurfacePresenter: HostSurface {
    fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer);
    fn forget(&mut self, surface: SurfaceId);

    fn observations(&self) -> Option<Arc<Mutex<Observations>>> {
        None
    }

    #[cfg(all(test, feature = "macos-surface"))]
    fn take_native_completions(&mut self) -> Vec<(SurfaceId, bool)> {
        Vec::new()
    }

    #[cfg(feature = "macos-surface")]
    fn attach_native(
        &mut self,
        _surface: SurfaceId,
        _content: hl_iosurface::Surface,
    ) -> Result<(), hl_iosurface::Surface> {
        Err(_content)
    }
}

impl SurfacePresenter for PngPresenter {
    fn deposit(&mut self, surface: SurfaceId, mut buffer: StoredBuffer) {
        if buffer.bgra {
            for pixel in buffer.rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            buffer.bgra = false;
        }
        PngPresenter::deposit(self, surface, buffer);
    }

    fn forget(&mut self, surface: SurfaceId) {
        PngPresenter::forget(self, surface);
    }

    fn observations(&self) -> Option<Arc<Mutex<Observations>>> {
        Some(PngPresenter::observations(self))
    }
}

#[cfg(all(target_os = "macos", feature = "macos-surface"))]
impl SurfacePresenter for crate::surface::macos::MacPresenter {
    fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        let mut bgra = buffer.rgba;
        if !buffer.bgra {
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        self.attach_bgra_damage(
            surface,
            bgra,
            buffer.width.max(1) as u32,
            buffer.height.max(1) as u32,
            buffer.damage,
        );
    }

    fn forget(&mut self, surface: SurfaceId) {
        crate::surface::macos::MacPresenter::forget(self, surface);
    }

    fn attach_native(
        &mut self,
        surface: SurfaceId,
        content: hl_iosurface::Surface,
    ) -> Result<(), hl_iosurface::Surface> {
        self.attach_iosurface(surface, content);
        Ok(())
    }
}

/// Type-erased presenter owned by the Smithay adapter. The composition root remains free to supply
/// any platform implementation of [`SurfacePresenter`].
pub struct AdapterPresenter {
    inner: Box<dyn SurfacePresenter>,
    observations: Arc<Mutex<Observations>>,
    #[cfg(feature = "macos-surface")]
    native: HashMap<SurfaceId, VecDeque<NativeAttachment>>,
    events: Vec<crate::scene::port::PresenterEvent>,
}

#[cfg(feature = "macos-surface")]
struct NativeAttachment {
    frame: Option<super::native::NativeFrame>,
    buffer: Option<BufferLease>,
    submission: Option<crate::scene::port::PresentationId>,
    submitted: bool,
}

#[cfg(feature = "macos-surface")]
enum BufferLease {
    Wayland(smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer),
    #[cfg(test)]
    Probe(Arc<std::sync::atomic::AtomicUsize>),
}

#[cfg(feature = "macos-surface")]
impl BufferLease {
    fn release(self) {
        match self {
            Self::Wayland(buffer) => buffer.release(),
            #[cfg(test)]
            Self::Probe(releases) => {
                releases.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

#[cfg(feature = "macos-surface")]
impl NativeAttachment {
    fn complete(mut self, outcome: super::native::NativeFrameOutcome) {
        if let Some(buffer) = self.buffer.take() {
            buffer.release();
        }
        if let Some(frame) = self.frame.take() {
            frame.complete(outcome);
        }
    }
}

#[cfg(feature = "macos-surface")]
impl Drop for NativeAttachment {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.release();
        }
    }
}

impl AdapterPresenter {
    #[cfg(feature = "macos-surface")]
    fn complete_native_submission(
        &mut self,
        completion: crate::scene::port::PresentationCompletion,
    ) {
        let matches = self
            .native
            .iter()
            .filter_map(|(&surface, attachments)| {
                attachments
                    .iter()
                    .position(|attachment| attachment.submission == Some(completion.id))
                    .map(|index| (surface, index))
            })
            .collect::<Vec<_>>();
        let outcome = match completion.outcome {
            crate::scene::port::CompletionOutcome::Delivered { .. } => {
                super::native::NativeFrameOutcome::Displayed
            }
            crate::scene::port::CompletionOutcome::TerminalFailure => {
                super::native::NativeFrameOutcome::TerminalFailure
            }
        };
        for (surface, index) in matches {
            let attachment = self
                .native
                .get_mut(&surface)
                .and_then(|attachments| attachments.remove(index));
            if self.native.get(&surface).is_some_and(VecDeque::is_empty) {
                self.native.remove(&surface);
            }
            if let Some(attachment) = attachment {
                attachment.complete(outcome);
            }
        }
    }

    #[cfg(feature = "macos-surface")]
    fn complete_unsubmitted(
        &mut self,
        surface: SurfaceId,
        outcome: super::native::NativeFrameOutcome,
    ) {
        let Some(attachments) = self.native.get_mut(&surface) else {
            return;
        };
        if let Some(index) = attachments
            .iter()
            .rposition(|attachment| attachment.submission.is_none())
        {
            if let Some(attachment) = attachments.remove(index) {
                attachment.complete(outcome);
            }
        }
        if attachments.is_empty() {
            self.native.remove(&surface);
        }
    }

    #[cfg(feature = "macos-surface")]
    fn supersede_native(&mut self, surface: SurfaceId, outcome: super::native::NativeFrameOutcome) {
        let Some(attachments) = self.native.get_mut(&surface) else {
            return;
        };
        while attachments
            .back()
            .is_some_and(|attachment| attachment.submission.is_none() && !attachment.submitted)
        {
            if let Some(attachment) = attachments.pop_back() {
                attachment.complete(outcome);
            }
        }
        if self.native.get(&surface).is_some_and(VecDeque::is_empty) {
            self.native.remove(&surface);
        }
    }

    pub fn new(presenter: impl SurfacePresenter + 'static) -> AdapterPresenter {
        let observations = presenter
            .observations()
            .unwrap_or_else(|| Arc::new(Mutex::new(Observations::default())));
        AdapterPresenter {
            inner: Box::new(presenter),
            observations,
            #[cfg(feature = "macos-surface")]
            native: HashMap::new(),
            events: Vec::new(),
        }
    }

    pub fn observations(&self) -> Arc<Mutex<Observations>> {
        Arc::clone(&self.observations)
    }

    pub fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        #[cfg(feature = "macos-surface")]
        self.supersede_native(surface, super::native::NativeFrameOutcome::Replaced);
        self.inner.deposit(surface, buffer);
    }

    #[cfg(feature = "macos-surface")]
    pub(crate) fn discard_native(&mut self) {
        let surfaces = self.native.keys().copied().collect::<Vec<_>>();
        for surface in surfaces {
            self.supersede_native(surface, super::native::NativeFrameOutcome::Discarded);
        }
    }

    pub fn forget(&mut self, surface: SurfaceId) {
        self.inner.forget(surface);
        #[cfg(feature = "macos-surface")]
        self.supersede_native(surface, super::native::NativeFrameOutcome::Discarded);
    }

    #[cfg(feature = "macos-surface")]
    pub(crate) fn attach_native(
        &mut self,
        surface: SurfaceId,
        frame: super::native::NativeFrame,
        buffer: Option<smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer>,
    ) -> Result<(), super::native::NativeFrame> {
        if self
            .inner
            .attach_native(surface, frame.surface.clone())
            .is_err()
        {
            if let Some(buffer) = buffer {
                buffer.release();
            }
            return Err(frame);
        }
        let attachment = NativeAttachment {
            frame: Some(frame),
            buffer: buffer.map(BufferLease::Wayland),
            submission: None,
            submitted: false,
        };
        let attachments = self.native.entry(surface).or_default();
        if attachments
            .back()
            .is_some_and(|attachment| attachment.submission.is_none() && !attachment.submitted)
        {
            if let Some(replaced) = attachments.pop_back() {
                replaced.complete(super::native::NativeFrameOutcome::Replaced);
            }
        }
        attachments.push_back(attachment);
        Ok(())
    }
}

impl<T> From<T> for AdapterPresenter
where
    T: SurfacePresenter + 'static,
{
    fn from(presenter: T) -> Self {
        AdapterPresenter::new(presenter)
    }
}

impl Presenter for AdapterPresenter {
    fn present_frame(&mut self, frame: &PresentFrame) -> PresentationFeedback {
        let feedback = self.inner.present_frame(frame);
        #[cfg(feature = "macos-surface")]
        match feedback.outcome {
            crate::scene::port::PresentOutcome::Pending { id } => {
                for layer in &frame.layers {
                    if let Some(attachment) = self
                        .native
                        .get_mut(&layer.image.surface)
                        .and_then(|frames| frames.back_mut())
                    {
                        attachment.submission = Some(id);
                        attachment.submitted = true;
                    }
                }
            }
            crate::scene::port::PresentOutcome::Delivered { .. } => {
                for layer in &frame.layers {
                    if let Some(attachment) = self
                        .native
                        .get_mut(&layer.image.surface)
                        .and_then(|frames| frames.back_mut())
                    {
                        attachment.submitted = true;
                    }
                }
            }
            crate::scene::port::PresentOutcome::TerminalFailure => {
                for layer in &frame.layers {
                    self.complete_unsubmitted(
                        layer.image.surface,
                        super::native::NativeFrameOutcome::TerminalFailure,
                    );
                }
            }
            crate::scene::port::PresentOutcome::Offscreen => {
                for layer in &frame.layers {
                    self.complete_unsubmitted(
                        layer.image.surface,
                        super::native::NativeFrameOutcome::Discarded,
                    );
                }
            }
            crate::scene::port::PresentOutcome::RetryableFailure => {}
        }
        feedback
    }

    fn cancel(&mut self, role: SurfaceId) {
        self.inner.cancel(role);
    }
}

impl HostEvents for AdapterPresenter {
    fn set_wake(&mut self, wake: Arc<dyn crate::scene::port::Wake>) {
        self.inner.set_wake(wake);
    }

    fn poll_events(&mut self) {
        self.inner.poll_events();
        for event in self.inner.take_events() {
            #[cfg(feature = "macos-surface")]
            if let crate::scene::port::PresenterEvent::Presentation(completion) = event {
                self.complete_native_submission(completion);
            }
            self.events.push(event);
        }
        #[cfg(all(test, feature = "macos-surface"))]
        for (surface, failed) in self.inner.take_native_completions() {
            let attachment = self.native.get_mut(&surface).and_then(VecDeque::pop_front);
            if self.native.get(&surface).is_some_and(VecDeque::is_empty) {
                self.native.remove(&surface);
            }
            if let Some(attachment) = attachment {
                attachment.complete(if failed {
                    super::native::NativeFrameOutcome::TerminalFailure
                } else {
                    super::native::NativeFrameOutcome::Displayed
                });
            }
        }
    }

    fn take_events(&mut self) -> Vec<crate::scene::port::PresenterEvent> {
        std::mem::take(&mut self.events)
    }
}

impl Clipboard for AdapterPresenter {
    fn set_clipboard_text(&mut self, text: &str) {
        self.inner.set_clipboard_text(text);
    }

    fn take_clipboard_text(&mut self) -> Option<String> {
        self.inner.take_clipboard_text()
    }
}

impl Windows for AdapterPresenter {
    fn reconcile_window(&mut self, window: &crate::scene::model::WindowState) {
        self.inner.reconcile_window(window);
    }

    fn destroy_window(&mut self, surface: SurfaceId) {
        #[cfg(feature = "macos-surface")]
        self.supersede_native(surface, super::native::NativeFrameOutcome::Discarded);
        self.inner.destroy_window(surface);
    }

    fn begin_interaction(
        &mut self,
        surface: SurfaceId,
        interaction: crate::scene::model::WindowInteraction,
    ) {
        self.inner.begin_interaction(surface, interaction);
    }

    fn set_cursor(&mut self, cursor: &crate::scene::port::HostCursor) {
        self.inner.set_cursor(cursor);
    }
}

#[cfg(test)]
#[path = "present/tests.rs"]
mod tests;
