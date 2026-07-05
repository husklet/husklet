//! The `Client` endpoint impls, split per Docker resource. Each submodule adds its own
//! `impl Client` block; they all attach to the single [`crate::Client`].

mod container;
mod image;
mod network;
mod system;
mod volume;
