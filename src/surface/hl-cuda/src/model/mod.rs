//! The CUDA object model + its invariants (OVERVIEW-v2 §2 `model/`).
//!
//! Pure owned values: no `Cmd` is built here and nothing is submitted. A [`context::CudaContext`]
//! aggregates the per-context state (device descriptor, allocation/module/stream tables, the pipeline
//! cache, the id counters); the [`super::service`] layer drives it and emits the IR.

pub mod context;
pub mod device;
pub mod event;
pub mod external_semaphore;
pub mod graph;
pub mod graphics;
pub mod memory;
pub mod module;
pub mod stream;
pub mod texture;
