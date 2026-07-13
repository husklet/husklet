# Rebrand exclusions and residue policy

An exhaustive search will contain legitimate `dd` substrings. This policy prevents both under-renaming and
damage caused by blind replacement.

## Must remain unchanged

- Docker API schema/field names and `docker.sock` basename;
- EGL/GLES, Vulkan, CUDA, libc, Linux syscall, Wayland and Metal API symbols/constants;
- `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, Vulkan loader variables and other external environment names;
- upstream/vendored Smithay and pinned `reference/**` source;
- cryptographic data, binary contents and protocol numeric tags unless a versioned protocol change is
  independently required;
- arbitrary fixture payloads where the literal is intentionally opaque to the system under test.

## Rename only with compatibility policy

- archive/sidecar filenames, xattr namespaces, state roots and cache/checkpoint identities;
- Mach/launchd/bundle identifiers and socket names;
- environment variables read by another process;
- FFI symbols, struct tags and wire magic identifiers;
- remote image references, release URLs and updater artifacts.

## Usually cosmetic, but still owned

Log prefixes, labels, comments, test temporary filenames, golden labels and debug dump directories may be
renamed after behavioral contracts. They should not block a risky wave or be used as evidence that a
cross-process rename is complete.

## Final residue manifest fields

For every surviving match record: literal, path, classification, owner, reason, removal/review date (if
temporary), and test proving compatibility. Broad directory exemptions are forbidden for project-owned
code. `reference/**`, `third_party/**` and historical Git data may be excluded at directory granularity.

Generated files list their generator as owner. A remaining generated old name is fixed at the generator,
not waived repeatedly in output files.
