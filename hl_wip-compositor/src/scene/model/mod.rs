//! The neutral scene MODEL — the values + invariants the compositor policy owns.
//!
//! No Smithay, no GPU, no platform types: a plain object graph (`Scene` of `Surface`s in a
//! window/subsurface/popup tree over `Output`s, with a `Seat`, `DamageRegion`s, and the
//! `PresentableImage`/`Positioner` value types) the `service/*` use-cases operate on through `port/*`.

pub mod damage;
pub mod output;
pub mod scene;
pub mod seat;
pub mod surface;
pub mod window;

pub use damage::{DamageRegion, Rect};
pub use output::{Output, OutputId};
pub use scene::Scene;
pub use seat::Seat;
pub use surface::{BufferState, Format, PresentableImage, Surface, SurfaceId, Viewport, Visibility};
pub use window::{
    Anchor, ConstraintAdjustment, Gravity, PopupPlacement, PopupState, Positioner, SubsurfaceState,
    SurfaceRole,
};
