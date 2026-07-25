# references/oracle — CUDA/NVML parity oracles

Non-Rust, tracked, first-party. The C clean-room shims that the Rust driver replaces
as the deploy artifact, kept here as behavioral parity oracles (OVERVIEW D1: Rust
cdylib ships, C = oracle).

Moves here:
- `cuda_shim.c` + `cuda_min.h` — clean-room libcuda.
- `cudart_shim.c` + `cudart_min.h` + `fatbin.h` — clean-room libcudart.
- `nvml_shim.c` + `nvml_min.h` — clean-room libnvidia-ml.
- `test_cuda.c`, `test_cudart.c`, `test_nvml.c` — conformance harnesses.

Parity tests run the Rust shim and the C oracle against the same harness and diff
results. Distinct from the gitignored top-level `reference/`. See OVERVIEW D5.
