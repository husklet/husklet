# Linux behavior fixtures

These C fixtures define observable guest behavior—results, output,
synchronization, descriptors, and process interactions—without inspecting
engine internals. Each manifest row owns its guest ISA, build flags, expected
result, and state:

- `active` cases must build and run in every selected lane.
- `excluded` cases require a concrete reason in the manifest.
- A runtime capability mismatch must fail the lane; fixtures must not silently
  turn missing coverage into success.

Expected files contain deterministic behavior only. Performance workloads stay
outside this tree so timing noise cannot weaken correctness gates. The broader,
manual upstream differential lane lives in `tests/compliance/ltp`.
