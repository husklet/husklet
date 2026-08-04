# Filesystem scenario oracle

These end-to-end cases preserve the images, commands, timeouts, and output
contracts formerly declared in `tests/scenarios/fixtures/filesystem-core.yaml`.
All 37 stable IDs have a one-to-one directory-local definition. Expected
output is now owned by this category instead of embedded in a shared fixture.

`test.yaml` is the sole declarative registration for this category. The
legacy manifest and its pure Rust loader/runner wrapper were removed only
after matching every ID, image, command, target restriction, class, timeout,
resource declaration, and output value.

This migration only changes test ownership and representation. It changes no
runtime implementation, so the retained C engine was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
