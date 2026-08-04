//! Overlay lookup, copy-up, mutation, and merged-directory semantics.

mod directory;
mod lookup;
mod model;
mod mutation;

pub use lookup::Overlay;
pub use model::*;

#[cfg(test)]
mod behavior_test;
