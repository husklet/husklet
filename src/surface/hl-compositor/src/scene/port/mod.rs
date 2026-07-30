//! The boundary traits the scene talks through — its inward contracts.
//!
//! Two small ports keep the whole policy testable with fakes: [`Presenter`] (where finished frames go)
//! and [`Clock`] (the monotonic time source). Everything platform — Cocoa/Metal, DRM/GBM, the host
//! clock — lives behind these; the neutral core depends only on the traits.

pub mod clock;
pub mod cursor;
pub mod presenter;

pub use clock::Clock;
pub use cursor::{CursorImage, CursorShape, HostCursor};
pub use presenter::{
    Clipboard, CompletionOutcome, HostEvents, HostSurface, PresentFrame, PresentLayer,
    PresentOutcome, PresentTiming, PresentationCompletion, PresentationFeedback, PresentationId,
    Presenter, PresenterEvent, ScrollSource, Wake, Windows,
};
