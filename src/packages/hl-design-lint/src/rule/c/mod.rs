//! Language-aware policy for repository-owned C, Objective-C, and assembly.

mod policy;
mod structure;

pub use policy::{CallPolicy, Policy};
pub use structure::Structure;
