//! Bounded component-relative guest path resolution.

mod error;
mod path;
mod pin;
mod resolve;

pub use error::*;
pub use resolve::*;

#[cfg(test)]
mod resolve_test;
