# references/registry — CUDA census sidecars

Non-Rust, tracked, first-party. The extractor + manifest sidecars that drive the
crate's census / anti-drift tests (the exported symbol sets in `shim/*/lib.rs` are
diffed against these).

Moves here:
- `extract_cuda_manifest.py` — pulls the canonical cu*/cuda*/nvml* symbol lists from
  the CUDA headers (`cuda_min.h`, `cudart_min.h`, `nvml_min.h`).
- `cuda_driver.manifest` — expected libcuda.so.1 exports.
- `cudart.manifest` — expected libcudart.so.1 exports.
- (nvml manifest as needed.)

Distinct from the top-level gitignored `reference/` (external upstream clones). See
OVERVIEW D5.
