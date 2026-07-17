//! The CPU-native sampler. The software oracle records a sampler's existence (and generation, for stale-
//! reference detection) but does not model filtering state beyond the blit path, so the native is a marker.
//! Ported from the `samplers: ResourceTable<()>` slot in `hl-gpu/src/software.rs`.

pub struct Sampler;
