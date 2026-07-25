//! The CPU executor's per-operation work: [`raster`] (render-pass clears + the software draw path),
//! [`copy`] (buffer/texture copies, blit, resolve, + the shared bounds/layout helpers), and [`compute`]
//! (dispatch → the neutral kernel interpreter). One operation family per file; the encoder-level
//! orchestration + validation lives in [`crate::cpu::executor`].

pub(crate) mod compute;
pub(crate) mod copy;
pub(crate) mod raster;
