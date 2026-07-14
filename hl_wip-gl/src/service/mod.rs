//! The GL operation seam (mirrors hl-cuda's `service/`), split by role rather than by API family.
//!
//! GL is **deferred-lowering**, so the seam has a different shape from cuda's submit-per-op layer:
//! * [`record`] — the `gl*` recording ops. Each takes `&mut GlContext`, mutates the model (creates a
//!   buffer/texture/program, binds state, appends a draw to the draw-list), and submits NOTHING. No
//!   `CommandSink` is threaded here — a `gl*` call emits no IR.
//! * [`query`] — the read-only `gl*Get*` introspection ops (identity strings, capability limits, bound
//!   state, shader/program status, uniform/attribute reflection). Pure: `&GlContext` in, values out; no
//!   IR, no mutation. The seam a real app polls during init and every frame.
//! * [`frame`] — `build_frame_ir`: turn the recorded draw-list into the frame's `Cmd` stream (resource
//!   creates/uploads + one `Submit` carrying the render-pass encoder ops). Pure: state in, `Cmd`s out.
//! * [`swap`] — `eglSwapBuffers`: the one sink-touching op. It builds the frame IR, submits it + a
//!   `Present` through the `&mut dyn CommandSink`, then resets the per-frame state. This is the tested
//!   lowering surface (a driver test drives it against a `hl_gpu::RecordingSink`).

pub mod frame;
pub mod query;
pub mod readpixels;
pub mod record;
pub mod swap;
