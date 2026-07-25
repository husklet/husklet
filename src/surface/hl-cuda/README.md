# hl-cuda

Guest CUDA, CUDA Runtime, and NVML implementation that lowers supported operations and PTX kernels
to neutral `hl-gpu` commands.

The crate owns CUDA models, validation, lowering, and guest shared libraries. It does not know about
containers or engine implementations. Husklet selects it as part of its graphics device, projects
the guest libraries, and supplies the workspace's typed CUDA configuration declaratively.

```text
shim/       guest CUDA, cudart, and NVML shared libraries
src/        API models, PTX adaptation, and GPU lowering
tests/      lowering and compatibility coverage
```

Build and test with:

```sh
cargo test -p hl-cuda
```
