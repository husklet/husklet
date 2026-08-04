//! Bounded, per-engine Linux AIO context ownership.

#![forbid(unsafe_code)]

mod catalog;
mod context;
mod values;

pub use catalog::{Catalog, CatalogLimits};
pub use context::{Admission, EventBatch};
pub use values::{AioError, ContextId, Event};

#[cfg(test)]
mod test;
