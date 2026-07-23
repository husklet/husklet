# hl-gl

Guest EGL/OpenGL ES implementation that records API state and lowers frames to neutral `hl-gpu`
commands at `eglSwapBuffers`.

The crate owns GL models, validation, lowering, and guest shared libraries. It does not know about
containers or engine implementations. Husklet selects it as part of its graphics device and
projects the built guest libraries through the container device contract.

```text
shim/       guest EGL/GLES shared libraries
src/model/  GL objects and state
src/service recording, frame lowering, and presentation
src/adapter shader translation
tests/      lowering and compatibility coverage
```

Build and test with:

```sh
cargo test -p hl-gl
```
